//! Promoting a session's status and tags from a lifecycle signal.
//!
//! A session announces where it is in its work cycle — a `weaver hook` from a
//! terminal agent, an ACP turn boundary from [`crate::acp`] — and that signal
//! has to become durable state: the `sessions.status` row, the attention and
//! idle tags the dashboard reads, and an event on the bus. That translation is
//! the same wherever the signal came from, so it lives here rather than in the
//! monitor loop that used to own it.

use serde_json::json;

use crate::db::now_iso;
use crate::db::Db;
use crate::events;
use crate::events::EventBus;
use crate::session::{self as session_mod, Session};
use weaver_core::tags;
use weaver_core::BoxFut;

/// The tag mutations a work-cycle hook implies: `(key, value)` where an empty
/// value clears the tag (absence is the calm/default state). `working` returns
/// the agent to calm (clearing both axes it might carry); the quiet signals stamp
/// the soothing `idle` mark. `None` for a kind that carries no work-cycle signal.
pub fn lifecycle_mutations(kind: &str) -> Option<&'static [(&'static str, &'static str)]> {
    const WORKING: &[(&str, &str)] = &[(tags::ATTENTION_KEY, ""), (tags::IDLE_KEY, "")];
    const RESTING: &[(&str, &str)] = &[(tags::IDLE_KEY, tags::IDLE_VALUE)];
    match kind {
        "working" => Some(WORKING),
        "waiting" | "idle" => Some(RESTING),
        _ => None,
    }
}

/// Reflect a work-cycle lifecycle edge (`working`/`waiting`/`idle`) onto `session`
/// and its branch: lift the status to `running` (idempotent, never overriding a
/// terminal state) and apply the tag mutations, recording only what actually
/// changed. Returns the new event watermark.
pub fn promote_lifecycle<'a>(
    db: &'a Db,
    bus: &'a EventBus,
    session: &'a Session,
    kind: &'a str,
) -> BoxFut<'a, Option<i64>> {
    Box::pin(promote_lifecycle_inner(db, bus, session, kind))
}

async fn promote_lifecycle_inner(
    db: &Db,
    bus: &EventBus,
    session: &Session,
    kind: &str,
) -> Option<i64> {
    let mutations = lifecycle_mutations(kind)?;
    let branch_id = session.branch_id.as_str();
    let status_changed = session.status != "running" && !session_mod::is_terminal(&session.status);

    // One transaction makes the observed session generation the fence for the
    // entire mechanical edge: status promotion, activity timestamp, turn count,
    // and tags. A terminal/user/handoff mutation that wins first makes this a
    // no-op; one that waits commits afterward and is therefore unambiguously
    // newer than every artifact of this hook.
    let mut tx = match weaver_core::db::begin_immediate(db).await {
        Ok(tx) => tx,
        Err(error) => {
            tracing::warn!(id = %session.id, %error, "lifecycle transaction failed to start");
            return events::max_id(db).await.ok();
        }
    };
    let increment = i64::from(kind == "working");
    let promoted = match sqlx::query_scalar::<_, i64>(
        "UPDATE sessions
         SET status = 'running',
             last_activity_at = ?,
             turn_count = turn_count + ?,
             mutation_revision = mutation_revision + 1
         WHERE id = ?
           AND mutation_revision = ?
           AND status = ?
           AND status IN ('created', 'running', 'orphaned')
         RETURNING turn_count",
    )
    .bind(now_iso())
    .bind(increment)
    .bind(&session.id)
    .bind(session.mutation_revision)
    .bind(&session.status)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(Some(turn_count)) => turn_count,
        Ok(None) => {
            let _ = tx.rollback().await;
            tracing::debug!(id = %session.id, kind, "discarding stale lifecycle edge");
            return events::max_id(db).await.ok();
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(id = %session.id, %error, "lifecycle CAS failed");
            return events::max_id(db).await.ok();
        }
    };

    let cap_note = (session.policy_turn_budget > 0
        && promoted > session.policy_turn_budget
        && session.class == "automation"
        && session.managed_by.is_none())
    .then(|| format!("turn cap ({}) reached", session.policy_turn_budget));
    let set_at = now_iso();
    let mut changed_tags = Vec::<(String, String, String)>::new();
    for &(key, value) in mutations {
        if cap_note.is_some() && key == tags::ATTENTION_KEY {
            continue;
        }
        let current = sqlx::query_scalar::<_, String>(
            "SELECT value FROM tags WHERE branch_id = ? AND key = ?",
        )
        .bind(branch_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        if current == value {
            continue;
        }
        let result = if value.is_empty() {
            sqlx::query("DELETE FROM tags WHERE branch_id = ? AND key = ?")
                .bind(branch_id)
                .bind(key)
                .execute(&mut *tx)
                .await
        } else {
            sqlx::query(
                "INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
                 VALUES (?, ?, ?, '', 'agent', ?)
                 ON CONFLICT(branch_id, key) DO UPDATE SET
                   value = excluded.value, note = excluded.note,
                   set_by = excluded.set_by, set_at = excluded.set_at",
            )
            .bind(branch_id)
            .bind(key)
            .bind(value)
            .bind(&set_at)
            .execute(&mut *tx)
            .await
        };
        if let Err(error) = result {
            let _ = tx.rollback().await;
            tracing::warn!(id = %session.id, %error, "lifecycle tag transaction failed");
            return events::max_id(db).await.ok();
        }
        changed_tags.push((key.to_string(), value.to_string(), String::new()));
    }
    if let Some(note) = &cap_note {
        let current = sqlx::query_scalar::<_, String>(
            "SELECT value FROM tags WHERE branch_id = ? AND key = ?",
        )
        .bind(branch_id)
        .bind(tags::ATTENTION_KEY)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        if current != "blocked" {
            if let Err(error) = sqlx::query(
                "INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
                 VALUES (?, ?, 'blocked', ?, 'agent', ?)
                 ON CONFLICT(branch_id, key) DO UPDATE SET
                   value = excluded.value, note = excluded.note,
                   set_by = excluded.set_by, set_at = excluded.set_at",
            )
            .bind(branch_id)
            .bind(tags::ATTENTION_KEY)
            .bind(note)
            .bind(&set_at)
            .execute(&mut *tx)
            .await
            {
                let _ = tx.rollback().await;
                tracing::warn!(id = %session.id, %error, "lifecycle cap transaction failed");
                return events::max_id(db).await.ok();
            }
            changed_tags.push((
                tags::ATTENTION_KEY.to_string(),
                "blocked".to_string(),
                note.clone(),
            ));
        }
    }
    if let Err(error) = tx.commit().await {
        tracing::warn!(id = %session.id, %error, "lifecycle transaction failed to commit");
        return events::max_id(db).await.ok();
    }

    if status_changed {
        let _ = events::record(
            db,
            bus,
            branch_id,
            "status",
            json!({ "status": "running", "source": "hook" }),
        )
        .await;
    }
    for (key, value, note) in changed_tags {
        let _ = events::record_tag(db, bus, branch_id, &key, &value, &note, "agent").await;
    }

    // Advance the watermark past our own freshly-recorded events so the next
    // tick doesn't reprocess them. `None` on a read error just leaves the
    // caller's watermark untouched (the consumed event is already accounted for).
    events::max_id(db).await.ok()
}

/// Drive an ACP session's status/idle from a turn boundary — the acp task calls
/// this at turn start (`kind = "working"`) and turn end (`kind = "idle"`). It
/// records the same `hook` event row `weaver hook --event <kind>` would (the
/// durable audit trail), then promotes the status/tags directly through the
/// shared [`promote_lifecycle`] path — bypassing [`apply_hook`]'s ACP filter,
/// which exists only to ignore stray user-authored work-cycle hooks. Best-effort:
/// a missing session or write error is logged upstream, never fatal to the turn.
pub async fn record_acp_lifecycle(db: &Db, bus: &EventBus, session_id: &str, kind: &str) {
    let session = match session_mod::get(db, session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "acp lifecycle: session lookup failed");
            return;
        }
    };
    if let Err(e) =
        events::record_local(db, &session.branch_id, "hook", json!({ "event": kind })).await
    {
        tracing::warn!(session = %session_id, error = %e, "acp lifecycle: hook audit write failed");
    }
    let _ = promote_lifecycle(db, bus, &session, kind).await;
}
