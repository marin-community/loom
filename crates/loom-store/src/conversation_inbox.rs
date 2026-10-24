//! Durable conversation input shared by user feedback, session updates, and reviews.
//!
//! Each input becomes an inbox item that a session's ACP task claims,
//! opens a turn against, and then settles. The claim is a durable lease: the row
//! carries the token, so a task that dies mid-turn releases its claim on restart
//! rather than wedging the lane. Session updates stay addressed to their exact
//! recipient; reviews may follow a replacement session on the branch.

use anyhow::{Context, Result};
use serde_json::Value;
use std::future::Future;

use crate::channel_data::Urgency;

pub fn update_interrupt_at(urgency: Urgency) -> Option<String> {
    let delay = match urgency {
        Urgency::Debug => return None,
        Urgency::Normal => chrono::TimeDelta::seconds(60),
        Urgency::Attention | Urgency::Blocked => chrono::TimeDelta::zero(),
    };
    Some(
        (chrono::Utc::now() + delay)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    )
}

pub fn session_update_text(source: &str, text: &str) -> String {
    format!("Update from loom session {source}\n\n{text}")
}

#[derive(Debug, sqlx::FromRow)]
pub struct InboxItem {
    pub delivery_key: String,
    pub review_id: Option<i64>,
    pub payload: String,
    pub author: Option<String>,
    pub resources: String,
    pub claim_token: String,
    pub source_session_id: Option<String>,
    pub source_channel_id: Option<String>,
    pub source_message_id: Option<String>,
}

pub async fn pending_inputs(
    db: &crate::db::Db,
    session: &str,
) -> Result<Vec<weaver_api::QueuedInputView>> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT delivery_key, source_session_id, source_channel_id, source_message_id,
                payload, state, interrupt_at, editable FROM conversation_inbox
         WHERE preferred_session_id = ? AND review_id IS NULL AND state = 'queued'
         ORDER BY created_at, rowid",
    )
    .bind(session)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(weaver_api::QueuedInputView {
                delivery_key: row.try_get("delivery_key")?,
                source_session_id: row.try_get("source_session_id")?,
                source_channel_id: row.try_get("source_channel_id")?,
                source_message_id: row.try_get("source_message_id")?,
                text: row.try_get("payload")?,
                state: row.try_get("state")?,
                interrupt_at: row.try_get("interrupt_at")?,
                editable: row.try_get("editable")?,
            })
        })
        .collect()
}

/// Explicitly queued adjacent user inputs can remain one editable draft. Other senders keep their
/// place in the queue, and claiming or editing a draft fences further appends.
pub async fn enqueue_prompt(
    db: &crate::db::Db,
    session: &str,
    text: &str,
    by: Option<&str>,
    resources: &[Value],
    append_to_draft: bool,
) -> Result<String> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let last: Option<(String, String, Option<String>, String, bool)> = sqlx::query_as(
        "SELECT delivery_key, payload, author, resources, editable FROM conversation_inbox
         WHERE preferred_session_id = ? AND review_id IS NULL AND state = 'queued'
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(session)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((key, previous, author, saved_resources, true)) = last {
        if append_to_draft && author.as_deref() == by {
            let mut combined_resources: Vec<Value> = serde_json::from_str(&saved_resources)?;
            combined_resources.extend_from_slice(resources);
            sqlx::query(
                "UPDATE conversation_inbox SET payload = ?, resources = ? WHERE delivery_key = ?",
            )
            .bind(format!("{}\n\n{text}", previous.trim()))
            .bind(serde_json::to_string(&combined_resources)?)
            .bind(&key)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(key);
        }
    }
    let key = format!("prompt:{session}:{}", weaver_core::branch::new_id());
    let inserted = sqlx::query(
        "INSERT INTO conversation_inbox
            (delivery_key, branch_id, preferred_session_id, payload, editable, author, resources)
         SELECT ?, branch_id, id, ?, 1, ?, ? FROM sessions WHERE id = ? AND status != 'archived'",
    )
    .bind(&key)
    .bind(text)
    .bind(by)
    .bind(serde_json::to_string(resources)?)
    .bind(session)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() != 1 {
        anyhow::bail!("session is missing or archived");
    }
    tx.commit().await?;
    Ok(key)
}

pub async fn retract_input(
    db: &crate::db::Db,
    session: &str,
    delivery_key: Option<&str>,
) -> Result<String> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let text: Option<String> = sqlx::query_scalar(
        "DELETE FROM conversation_inbox WHERE delivery_key = (
             SELECT delivery_key FROM conversation_inbox
             WHERE preferred_session_id = ? AND state = 'queued' AND editable = 1
               AND review_id IS NULL AND (? IS NULL OR delivery_key = ?)
             ORDER BY created_at DESC, rowid DESC LIMIT 1
         ) RETURNING payload",
    )
    .bind(session)
    .bind(delivery_key)
    .bind(delivery_key)
    .fetch_optional(&mut *tx)
    .await?;
    let text = text.ok_or_else(|| anyhow::anyhow!("there is no editable queued input"))?;
    tx.commit().await?;
    Ok(text)
}

#[derive(Clone, Copy)]
enum InboxLane<'a> {
    Review,
    Session {
        interrupt_only: bool,
        delivery_key: Option<&'a str>,
    },
}

pub async fn enqueue_session_update(
    db: &crate::db::Db,
    session: &str,
    source: &str,
    text: &str,
    key: Option<&str>,
) -> Result<String> {
    let key = format!(
        "session:{source}:{session}:{}",
        key.map(str::to_owned)
            .unwrap_or_else(weaver_core::branch::new_id)
    );
    let payload = session_update_text(source, text);
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT payload FROM conversation_inbox WHERE delivery_key = ?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
    if existing
        .as_ref()
        .is_some_and(|existing| existing != &payload)
    {
        anyhow::bail!("idempotency key already belongs to a different session update");
    }
    let inserted = sqlx::query(
        "INSERT INTO conversation_inbox (delivery_key, branch_id, preferred_session_id, payload, source_session_id, interrupt_at)
         SELECT ?, branch_id, id, ?, ?, ? FROM sessions WHERE id = ? AND status != 'archived'
         ON CONFLICT(delivery_key) DO NOTHING",
    ).bind(&key).bind(payload).bind(source).bind(update_interrupt_at(Urgency::Normal))
        .bind(session).execute(&mut *tx).await?;
    if existing.is_none() && inserted.rows_affected() == 0 {
        anyhow::bail!("session is missing or archived");
    }
    tx.commit().await?;
    Ok(key)
}

/// The durable state of one inbox delivery, if the entry exists — `queued`
/// means the protected entry has never been drained by a runtime.
pub async fn delivery_state(db: &crate::db::Db, delivery_key: &str) -> Result<Option<String>> {
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM conversation_inbox WHERE delivery_key = ?")
            .bind(delivery_key)
            .fetch_optional(db)
            .await?;
    Ok(state)
}

/// A source message returned by an agent read or wait is already in its context.
/// Only queued notices are consumed; a concurrent claimed turn keeps ownership.
pub async fn consume_source(db: &crate::db::Db, session: &str, message: &str) -> Result<()> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let keys: Vec<String> = sqlx::query_scalar(
        "UPDATE conversation_inbox SET state = 'consumed', consumed_at = ?
         WHERE preferred_session_id = ? AND (source_message_id = ? OR delivery_key = ?)
           AND review_id IS NULL AND state = 'queued' RETURNING delivery_key",
    )
    .bind(crate::db::now_iso())
    .bind(session)
    .bind(message)
    .bind(format!("channel:{message}:{session}"))
    .fetch_all(&mut *tx)
    .await?;
    for key in keys {
        sqlx::query(
            "UPDATE channel_deliveries SET state = 'read', updated_at = ?
             WHERE 'channel:' || message_id || ':' || target_session_id = ?",
        )
        .bind(crate::db::now_iso())
        .bind(key)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn claim_input(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
    claim_owner: Option<&str>,
    owner_is_live: impl Fn(&str, &str) -> bool,
    interrupt_only: bool,
    delivery_key: Option<&str>,
) -> Result<Option<InboxItem>> {
    claim_at(
        db,
        branch_id,
        session_id,
        claim_owner,
        owner_is_live,
        chrono::Utc::now(),
        InboxLane::Session {
            interrupt_only,
            delivery_key,
        },
    )
    .await
}

pub async fn has_due_input(
    db: &crate::db::Db,
    session: &str,
    exclude_key: Option<&str>,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_inbox
         WHERE preferred_session_id = ? AND review_id IS NULL
           AND state IN ('queued', 'delivering') AND interrupt_at <= ?
           AND (? IS NULL OR delivery_key != ?))",
    )
    .bind(session)
    .bind(crate::db::now_iso())
    .bind(exclude_key)
    .bind(exclude_key)
    .fetch_one(db)
    .await?)
}

pub struct InboxTurnBoundary<'a> {
    pub turn: i64,
    pub seq: i64,
    pub opening_payload: &'a Value,
    pub inflight: &'a str,
}

#[derive(Debug)]
pub enum InboxTurnStartOutcome {
    ClaimLostBeforeWrite,
    TransportNotWritten { error: anyhow::Error },
    Persisted,
    TransportWrittenUnpersisted { error: anyhow::Error },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxClaimSettlement {
    MatchingPromptResponse,
    UserCancelled,
    Abandoned,
}

impl InboxTurnStartOutcome {
    /// Once the transport write succeeds, the task must hold a local live-turn
    /// boundary even if journaling fails. Returning true is the dispatch gate:
    /// neither the ordinary prompt queue nor another inbox claim may start.
    pub fn blocks_followup_dispatch(&self) -> bool {
        matches!(
            self,
            Self::Persisted | Self::TransportWrittenUnpersisted { .. }
        )
    }
}

/// Claim the oldest immutable submitted-review message for this branch. The
/// branch address lets a replacement ACP session recover feedback that was
/// originally aimed at an unavailable predecessor.
pub async fn claim_review(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
    claim_owner: Option<&str>,
    owner_is_live: impl Fn(&str, &str) -> bool,
) -> Result<Option<InboxItem>> {
    claim_review_at(
        db,
        branch_id,
        session_id,
        claim_owner,
        owner_is_live,
        chrono::Utc::now(),
    )
    .await
}

#[doc(hidden)]
pub async fn claim_review_at(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
    claim_owner: Option<&str>,
    owner_is_live: impl Fn(&str, &str) -> bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<InboxItem>> {
    claim_at(
        db,
        branch_id,
        session_id,
        claim_owner,
        owner_is_live,
        now,
        InboxLane::Review,
    )
    .await
}

async fn claim_at(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
    claim_owner: Option<&str>,
    owner_is_live: impl Fn(&str, &str) -> bool,
    now: chrono::DateTime<chrono::Utc>,
    lane: InboxLane<'_>,
) -> Result<Option<InboxItem>> {
    let stale = now
        .checked_sub_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(crate::db::now_iso);
    let now = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let delivery_key = match lane {
        InboxLane::Session { delivery_key, .. } => delivery_key,
        InboxLane::Review => None,
    };
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let candidates: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT inbox.delivery_key, inbox.state,
                inbox.claimed_session_id, inbox.claim_owner
         FROM conversation_inbox AS inbox
         WHERE inbox.branch_id = ?
           AND CASE WHEN ? THEN inbox.review_id IS NOT NULL
                    ELSE inbox.review_id IS NULL AND inbox.preferred_session_id = ? END
           AND (NOT ? OR inbox.interrupt_at <= ?)
           AND (? IS NULL OR inbox.delivery_key = ?)
           AND (
             inbox.state = 'queued'
             OR (
               inbox.state = 'delivering'
               AND inbox.claimed_at <= ?
               AND NOT EXISTS (
                 SELECT 1 FROM sessions AS target
                 WHERE target.id = inbox.claimed_session_id
                   AND json_extract(target.acp_inflight, '$.delivery_key')
                       = inbox.delivery_key
               )
             )
           )
         ORDER BY inbox.created_at, inbox.rowid",
    )
    .bind(branch_id)
    .bind(matches!(lane, InboxLane::Review))
    .bind(session_id)
    .bind(matches!(
        lane,
        InboxLane::Session {
            interrupt_only: true,
            ..
        }
    ))
    .bind(&now)
    .bind(delivery_key)
    .bind(delivery_key)
    .bind(stale)
    .fetch_all(&mut *tx)
    .await?;
    let key = candidates
        .into_iter()
        .find_map(|(key, state, owner_session, owner)| {
            if state == "queued"
                || !owner_session
                    .as_deref()
                    .zip(owner.as_deref())
                    .is_some_and(|(session, owner)| owner_is_live(session, owner))
            {
                Some(key)
            } else {
                None
            }
        });
    let item = match key {
        Some(key) => {
            sqlx::query_as::<_, InboxItem>(
                "UPDATE conversation_inbox
                 SET state = 'delivering',
                     claimed_session_id = ?,
                     claim_token = lower(hex(randomblob(16))),
                     claim_owner = ?,
                     claimed_at = ?
                 WHERE delivery_key = ?
                 RETURNING delivery_key, review_id, payload, author, resources, claim_token,
                           source_session_id, source_channel_id, source_message_id",
            )
            .bind(session_id)
            .bind(claim_owner)
            .bind(now)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    tx.commit().await?;
    Ok(item)
}

async fn persist_conversation_inbox_turn(
    db: &crate::db::Db,
    item: &InboxItem,
    session_id: &str,
    boundary: InboxTurnBoundary<'_>,
) -> Result<()> {
    let now = crate::db::now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let still_owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM conversation_inbox
           WHERE delivery_key = ? AND state = 'delivering'
             AND claim_token = ? AND claimed_session_id = ?
         )",
    )
    .bind(&item.delivery_key)
    .bind(&item.claim_token)
    .bind(session_id)
    .fetch_one(&mut *tx)
    .await?;
    if !still_owned {
        tx.rollback().await?;
        anyhow::bail!("protected inbox claim was lost after the relay write");
    }
    let journaled = sqlx::query(
        "INSERT INTO chat_blocks (session_id, turn, seq, kind, payload, created_at)
         VALUES (?, ?, ?, 'user_message', ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(session_id)
    .bind(boundary.turn)
    .bind(boundary.seq)
    .bind(boundary.opening_payload.to_string())
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    if journaled.rows_affected() != 1 {
        anyhow::bail!("protected inbox turn journal position is already occupied");
    }
    // The turn-start fence, at the durable claim: a lifecycle teardown that
    // owns the session must not get a protected turn persisted under it, and
    // a dormant row outside its own revival (waking/adopting) has no runtime
    // to run one. One predicate shared with `session::set_inflight`, so the
    // allow/deny lists cannot drift.
    let session = sqlx::query(&format!(
        "UPDATE sessions SET acp_inflight = ? WHERE id = ? AND {}",
        crate::session::turn_fence_predicate()
    ))
    .bind(boundary.inflight)
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
    if session.rows_affected() != 1 {
        anyhow::bail!("protected inbox turn was fenced out: the session changed lifecycle state");
    }
    sqlx::query(
        "UPDATE channel_deliveries SET state = 'delivered', attempts = attempts + 1,
             last_error = NULL, updated_at = ?
         WHERE 'channel:' || message_id || ':' || target_session_id = ?",
    )
    .bind(&now)
    .bind(&item.delivery_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Hand protected feedback to the ACP socket, then persist its opening journal
/// block and in-flight prompt. The exact live task that owns the durable inbox
/// claim fences stale recovery even if this persistence step fails. The
/// explicit outcome separates a transport failure (safe to release/retry) from
/// a successful write whose journal failed (adopt until its matching response).
/// The socket has no durable supervisor acknowledgement, so a crash remains
/// recoverable at-least-once and may duplicate process delivery.
pub async fn start_conversation_inbox_turn<F>(
    db: &crate::db::Db,
    item: &InboxItem,
    session_id: &str,
    boundary: InboxTurnBoundary<'_>,
    transport: F,
) -> InboxTurnStartOutcome
where
    F: Future<Output = Result<()>>,
{
    let owns_claim = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
           SELECT 1 FROM conversation_inbox
           WHERE delivery_key = ? AND state = 'delivering'
             AND claim_token = ? AND claimed_session_id = ?
         )",
    )
    .bind(&item.delivery_key)
    .bind(&item.claim_token)
    .bind(session_id)
    .fetch_one(db)
    .await;
    let owns_claim = match owns_claim {
        Ok(owns_claim) => owns_claim,
        Err(error) => {
            return InboxTurnStartOutcome::TransportNotWritten {
                error: error.into(),
            };
        }
    };
    if !owns_claim {
        return InboxTurnStartOutcome::ClaimLostBeforeWrite;
    }

    // The turn-start fence, checked before the wire write: a lifecycle
    // teardown that owns the session must never receive the turn at all.
    // The persist below re-fences the durable claim; this keeps the relay
    // clean, and the not-written outcome releases the claim so the item
    // stays queued for the next wake.
    match crate::session::turn_start_allowed(db, session_id).await {
        Ok(true) => {}
        Ok(false) => {
            return InboxTurnStartOutcome::TransportNotWritten {
                error: anyhow::anyhow!(
                    "the session changed lifecycle state; the protected turn was fenced out"
                ),
            };
        }
        Err(error) => {
            return InboxTurnStartOutcome::TransportNotWritten { error };
        }
    }

    if let Err(error) = transport
        .await
        .context("writing protected inbox feedback to ACP relay")
    {
        return InboxTurnStartOutcome::TransportNotWritten { error };
    }

    match persist_conversation_inbox_turn(db, item, session_id, boundary).await {
        Ok(()) => InboxTurnStartOutcome::Persisted,
        Err(error) => InboxTurnStartOutcome::TransportWrittenUnpersisted { error },
    }
}

pub async fn release_conversation_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    sqlx::query(
        "UPDATE sessions
         SET acp_inflight = NULL
         WHERE id = (
           SELECT claimed_session_id FROM conversation_inbox
           WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?
         )
           AND json_extract(acp_inflight, '$.delivery_key') = ?
           AND json_extract(acp_inflight, '$.review_claim_token') = ?",
    )
    .bind(delivery_key)
    .bind(claim_token)
    .bind(delivery_key)
    .bind(claim_token)
    .execute(&mut *tx)
    .await?;
    let released = sqlx::query(
        "UPDATE conversation_inbox
         SET state = CASE WHEN review_id IS NULL AND EXISTS (
                 SELECT 1 FROM chat_blocks WHERE json_extract(payload, '$.delivery_key') = conversation_inbox.delivery_key
             ) THEN 'consumed' ELSE 'queued' END, claimed_session_id = NULL,
             claim_token = NULL, claim_owner = NULL, claimed_at = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(delivery_key)
    .bind(claim_token)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(released.rows_affected() == 1)
}

pub async fn consume_conversation_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
    session_id: &str,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let consumed = sqlx::query(
        "UPDATE conversation_inbox
         SET state = 'consumed', consumed_at = ?, claimed_session_id = ?,
             claim_token = NULL, claim_owner = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(crate::db::now_iso())
    .bind(session_id)
    .bind(delivery_key)
    .bind(claim_token)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if consumed {
        sqlx::query(
            "UPDATE sessions SET acp_inflight = NULL
             WHERE id = ?
               AND json_extract(acp_inflight, '$.delivery_key') = ?
               AND json_extract(acp_inflight, '$.review_claim_token') = ?",
        )
        .bind(session_id)
        .bind(delivery_key)
        .bind(claim_token)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(consumed)
}

pub async fn settle_conversation_inbox_claim(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
    session_id: &str,
    settlement: InboxClaimSettlement,
) -> Result<bool> {
    match settlement {
        InboxClaimSettlement::MatchingPromptResponse | InboxClaimSettlement::UserCancelled => {
            consume_conversation_inbox(db, delivery_key, claim_token, session_id).await
        }
        InboxClaimSettlement::Abandoned => {
            release_conversation_inbox(db, delivery_key, claim_token).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn claimed_inbox() -> (crate::db::Db, InboxItem) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/review-inbox", "main")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions
                (id, branch_id, work_dir, term_session, status, protocol)
             VALUES ('acp-review', ?, '/repo', 'relay', 'running', 'acp')",
        )
        .bind(&branch.id)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO reviews
                (id, repo_root, branch_id, session_id, subject_kind, subject_id,
                 subject_key, subject_label, subject_version, status, created_by,
                 delivery_state, delivery_key)
             VALUES
                (1, '/repo', ?, 'acp-review', 'artifact', '1',
                 'design', 'design', '1', 'submitted', 'alice',
                 'delivered', 'review:stable')",
        )
        .bind(&branch.id)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO conversation_inbox
                (delivery_key, review_id, branch_id, preferred_session_id, payload)
             VALUES ('review:stable', 1, ?, 'acp-review', 'immutable')",
        )
        .bind(&branch.id)
        .execute(&db)
        .await
        .unwrap();
        let item = claim_review(&db, &branch.id, "acp-review", Some("owner-a"), |_, _| false)
            .await
            .unwrap()
            .unwrap();
        (db, item)
    }

    #[tokio::test]
    async fn session_update_deadlines_survive_retries_and_release() {
        let (db, _) = claimed_inbox().await;
        let key = enqueue_session_update(&db, "acp-review", "child", "done", Some("deadline"))
            .await
            .unwrap();
        let branch: String =
            sqlx::query_scalar("SELECT branch_id FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        let deadline: String = sqlx::query_scalar(
            "SELECT interrupt_at FROM conversation_inbox WHERE delivery_key = ?",
        )
        .bind(&key)
        .fetch_one(&db)
        .await
        .unwrap();
        let due = chrono::DateTime::parse_from_rfc3339(&deadline)
            .unwrap()
            .with_timezone(&chrono::Utc);
        enqueue_session_update(&db, "acp-review", "child", "done", Some("deadline"))
            .await
            .unwrap();
        for (now, expected) in [
            (due - chrono::TimeDelta::milliseconds(1), false),
            (due, true),
        ] {
            let item = claim_at(
                &db,
                &branch,
                "acp-review",
                Some("owner"),
                |_, _| false,
                now,
                InboxLane::Session {
                    interrupt_only: true,
                    delivery_key: None,
                },
            )
            .await
            .unwrap();
            assert_eq!(item.is_some(), expected);
            if let Some(item) = item {
                release_conversation_inbox(&db, &item.delivery_key, &item.claim_token)
                    .await
                    .unwrap();
            }
        }
        let retained: String = sqlx::query_scalar(
            "SELECT interrupt_at FROM conversation_inbox WHERE delivery_key = ?",
        )
        .bind(key)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(retained, deadline);
    }

    #[tokio::test]
    async fn due_claim_skips_debug_and_future_updates_but_idle_claim_is_fifo() {
        let (db, _) = claimed_inbox().await;
        let branch: String =
            sqlx::query_scalar("SELECT branch_id FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        let debug = enqueue_session_update(&db, "acp-review", "child", "debug", Some("debug"))
            .await
            .unwrap();
        sqlx::query("UPDATE conversation_inbox SET interrupt_at = NULL WHERE delivery_key = ?")
            .bind(&debug)
            .execute(&db)
            .await
            .unwrap();
        let future = enqueue_session_update(&db, "acp-review", "child", "future", Some("future"))
            .await
            .unwrap();
        let due = enqueue_session_update(&db, "acp-review", "child", "due", Some("due"))
            .await
            .unwrap();
        sqlx::query("UPDATE conversation_inbox SET interrupt_at = '2000-01-01T00:00:00.000Z' WHERE delivery_key = ?")
            .bind(&due).execute(&db).await.unwrap();
        assert!(has_due_input(&db, "acp-review", None).await.unwrap());
        let item = claim_input(&db, &branch, "acp-review", None, |_, _| false, true, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.delivery_key, due);
        consume_conversation_inbox(&db, &item.delivery_key, &item.claim_token, "acp-review")
            .await
            .unwrap();
        assert!(!has_due_input(&db, "acp-review", None).await.unwrap());
        for expected in [debug, future] {
            let item = claim_input(&db, &branch, "acp-review", None, |_, _| false, false, None)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(item.delivery_key, expected);
            consume_conversation_inbox(&db, &item.delivery_key, &item.claim_token, "acp-review")
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn user_and_agent_inputs_share_ordering_claims_and_edit_protection() {
        let (db, review) = claimed_inbox().await;
        let first = enqueue_prompt(
            &db,
            "acp-review",
            "first",
            Some("manual"),
            &[json!({"name": "code.rs"})],
            true,
        )
        .await
        .unwrap();
        let appended = enqueue_prompt(&db, "acp-review", "second", Some("manual"), &[], true)
            .await
            .unwrap();
        assert_eq!(first, appended);
        let agent = enqueue_session_update(&db, "acp-review", "child", "review ready", None)
            .await
            .unwrap();
        let last = enqueue_prompt(&db, "acp-review", "third", Some("manual"), &[], true)
            .await
            .unwrap();
        let pending = pending_inputs(&db, "acp-review").await.unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|input| input.delivery_key.as_str())
                .collect::<Vec<_>>(),
            vec![first.as_str(), agent.as_str(), last.as_str()]
        );
        assert_eq!(pending[0].text, "first\n\nsecond");
        assert!(retract_input(&db, "acp-review", Some(&agent))
            .await
            .is_err());
        assert!(retract_input(&db, "acp-review", Some(&review.delivery_key))
            .await
            .is_err());
        assert_eq!(
            retract_input(&db, "acp-review", None).await.unwrap(),
            "third"
        );
        let branch: String =
            sqlx::query_scalar("SELECT branch_id FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        let claimed = claim_input(&db, &branch, "acp-review", None, |_, _| false, false, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.delivery_key, first);
        assert_eq!(claimed.author.as_deref(), Some("manual"));
        assert_eq!(
            serde_json::from_str::<Vec<Value>>(&claimed.resources).unwrap(),
            vec![json!({"name": "code.rs"})]
        );
        assert!(retract_input(&db, "acp-review", Some(&first))
            .await
            .is_err());
        consume_conversation_inbox(&db, &first, &claimed.claim_token, "acp-review")
            .await
            .unwrap();
        let claimed = claim_input(&db, &branch, "acp-review", None, |_, _| false, false, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.delivery_key, agent);
        assert_eq!(claimed.source_session_id.as_deref(), Some("child"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn editing_queued_input_waits_for_a_concurrent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::connect(&dir.path().join("weaver.db"))
            .await
            .unwrap();
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/busy-queue", "main")
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, branch_id, work_dir, term_session, status) VALUES ('busy-queue', ?, '/repo', 'relay', 'running')").bind(&branch.id).execute(&db).await.unwrap();
        enqueue_prompt(
            &db,
            "busy-queue",
            "send after stop",
            Some("manual"),
            &[],
            true,
        )
        .await
        .unwrap();
        let writer = weaver_core::db::begin_immediate(&db).await.unwrap();
        let contender = db.clone();
        let editing =
            tokio::spawn(async move { retract_input(&contender, "busy-queue", None).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!editing.is_finished());
        writer.commit().await.unwrap();
        let text = tokio::time::timeout(std::time::Duration::from_secs(1), editing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(text, "send after stop");
    }

    #[tokio::test]
    async fn concurrent_updates_keep_one_identity_and_reject_changed_retries() {
        let (db, _) = claimed_inbox().await;
        let send = || enqueue_session_update(&db, "acp-review", "child", "done", Some("result:1"));
        let (first, retry) = tokio::join!(send(), send());
        assert_eq!(first.unwrap(), retry.unwrap());
        assert!(
            enqueue_session_update(&db, "acp-review", "child", "changed", Some("result:1"))
                .await
                .is_err()
        );
        enqueue_session_update(&db, "acp-review", "child", "done", Some("result:2"))
            .await
            .unwrap();
        let pending = pending_inputs(&db, "acp-review").await.unwrap();
        assert_eq!(
            pending.len(),
            2,
            "distinct publications may have identical text"
        );
        assert_eq!(pending[0].source_session_id.as_deref(), Some("child"));
        assert!(retract_input(&db, "acp-review", None).await.is_err());
        assert_eq!(pending_inputs(&db, "acp-review").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn journaled_session_update_is_not_replayed_after_abandonment() {
        let (db, review) = claimed_inbox().await;
        consume_conversation_inbox(&db, &review.delivery_key, &review.claim_token, "acp-review")
            .await
            .unwrap();
        enqueue_session_update(&db, "acp-review", "child", "done", Some("result:1"))
            .await
            .unwrap();
        let branch: String =
            sqlx::query_scalar("SELECT branch_id FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(claim_input(
            &db,
            &branch,
            "other-session",
            Some("other"),
            |_, _| false,
            false,
            None,
        )
        .await
        .unwrap()
        .is_none());
        let item = claim_input(
            &db,
            &branch,
            "acp-review",
            Some("driver"),
            |_, _| false,
            false,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let payload = json!({ "text": item.payload, "delivery_key": item.delivery_key });
        let inflight =
            json!({ "delivery_key": item.delivery_key, "review_claim_token": item.claim_token })
                .to_string();
        let outcome = start_conversation_inbox_turn(
            &db,
            &item,
            "acp-review",
            InboxTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &payload,
                inflight: &inflight,
            },
            async { Ok(()) },
        )
        .await;
        assert!(matches!(outcome, InboxTurnStartOutcome::Persisted));
        settle_conversation_inbox_claim(
            &db,
            &item.delivery_key,
            &item.claim_token,
            "acp-review",
            InboxClaimSettlement::Abandoned,
        )
        .await
        .unwrap();
        assert!(claim_input(
            &db,
            &branch,
            "acp-review",
            Some("replacement"),
            |_, _| { false },
            false,
            None,
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn post_write_persistence_failure_holds_every_followup_until_prompt_response() {
        let (db, item) = claimed_inbox().await;
        enqueue_prompt(
            &db,
            "acp-review",
            "ordinary queued prompt",
            Some("manual"),
            &[],
            true,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_review_inflight
             BEFORE UPDATE OF acp_inflight ON sessions
             BEGIN SELECT RAISE(ABORT, 'injected failure after journal insert'); END",
        )
        .execute(&db)
        .await
        .unwrap();
        let payload = json!({
            "text": item.payload,
            "by": Value::Null,
            "resources": [],
            "delivery_key": item.delivery_key,
        });
        let outcome = start_conversation_inbox_turn(
            &db,
            &item,
            "acp-review",
            InboxTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &payload,
                inflight: r#"{"prompt_id":7,"turn":1,"delivery_key":"review:stable"}"#,
            },
            async { Ok(()) },
        )
        .await;
        let InboxTurnStartOutcome::TransportWrittenUnpersisted { error } = &outcome else {
            panic!("expected a written but unpersisted outcome: {outcome:?}");
        };
        assert!(error.to_string().contains("injected failure"));
        assert!(
            outcome.blocks_followup_dispatch(),
            "the caller must adopt the ambiguous live-turn boundary"
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(state, "delivering");
        let blocks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_blocks")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            blocks, 0,
            "the failed transaction must not leak a journal marker"
        );
        let ordinary_started = if outcome.blocks_followup_dispatch() {
            None
        } else {
            Some(retract_input(&db, "acp-review", None).await.unwrap())
        };
        assert!(
            ordinary_started.is_none(),
            "the dispatch caller must not drain an ordinary prompt after the relay write"
        );
        assert_eq!(
            pending_inputs(&db, "acp-review").await.unwrap()[0].text,
            "ordinary queued prompt"
        );
        let branch_id: String =
            sqlx::query_scalar("SELECT branch_id FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        let after_stale_cutoff = chrono::Utc::now() + chrono::TimeDelta::minutes(2);
        assert!(
            claim_review_at(
                &db,
                &branch_id,
                "acp-other",
                Some("owner-b"),
                |session, owner| session == "acp-review" && owner == "owner-a",
                after_stale_cutoff,
            )
            .await
            .unwrap()
            .is_none(),
            "the ambiguous write stays unavailable past the age cutoff while its task is live"
        );

        sqlx::query("DROP TRIGGER fail_review_inflight")
            .execute(&db)
            .await
            .unwrap();
        assert!(consume_conversation_inbox(
            &db,
            &item.delivery_key,
            &item.claim_token,
            "acp-review"
        )
        .await
        .unwrap());
        assert_eq!(
            retract_input(&db, "acp-review", None).await.unwrap(),
            "ordinary queued prompt",
            "the ordinary queue becomes eligible only after the matching response"
        );
    }

    #[tokio::test]
    async fn abandoned_turn_settlement_requeues_the_real_claim_boundary() {
        let (db, item) = claimed_inbox().await;
        sqlx::query("UPDATE sessions SET acp_inflight = ? WHERE id = 'acp-review'")
            .bind(format!(
                r#"{{"delivery_key":"{}","review_claim_token":"{}"}}"#,
                item.delivery_key, item.claim_token
            ))
            .execute(&db)
            .await
            .unwrap();

        assert!(settle_conversation_inbox_claim(
            &db,
            &item.delivery_key,
            &item.claim_token,
            "acp-review",
            InboxClaimSettlement::Abandoned,
        )
        .await
        .unwrap());
        let row: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT state, claim_owner, claim_token
             FROM conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row, ("queued".to_string(), None, None));
        let inflight: Option<String> =
            sqlx::query_scalar("SELECT acp_inflight FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(inflight.is_none());
    }

    #[tokio::test]
    async fn relay_write_failure_keeps_protected_feedback_recoverable() {
        let (db, item) = claimed_inbox().await;
        let outcome = start_conversation_inbox_turn(
            &db,
            &item,
            "acp-review",
            InboxTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &json!({
                    "text": item.payload,
                    "delivery_key": item.delivery_key,
                }),
                inflight: r#"{"prompt_id":7,"turn":1,"delivery_key":"review:stable"}"#,
            },
            async { Err(anyhow::anyhow!("injected relay write failure")) },
        )
        .await;
        let InboxTurnStartOutcome::TransportNotWritten { error } = outcome else {
            panic!("expected an unwritten transport failure: {outcome:?}");
        };
        assert!(format!("{error:#}").contains("injected relay write failure"));

        let state: String = sqlx::query_scalar(
            "SELECT state FROM conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(state, "delivering");
        let blocks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_blocks")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(blocks, 0);
        let inflight: Option<String> =
            sqlx::query_scalar("SELECT acp_inflight FROM sessions WHERE id = 'acp-review'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(inflight.is_none());

        assert!(
            release_conversation_inbox(&db, &item.delivery_key, &item.claim_token)
                .await
                .unwrap()
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(state, "queued");
    }

    #[tokio::test]
    async fn lost_claim_cannot_create_a_turn_or_complete_the_inbox() {
        let (db, item) = claimed_inbox().await;
        sqlx::query(
            "UPDATE conversation_inbox
             SET claim_token = 'new-owner' WHERE delivery_key = ?",
        )
        .bind(&item.delivery_key)
        .execute(&db)
        .await
        .unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let observed_writes = writes.clone();
        let outcome = start_conversation_inbox_turn(
            &db,
            &item,
            "acp-review",
            InboxTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &json!({
                    "text": item.payload,
                    "delivery_key": item.delivery_key,
                }),
                inflight: r#"{"prompt_id":7,"turn":1}"#,
            },
            async move {
                observed_writes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(matches!(
            outcome,
            InboxTurnStartOutcome::ClaimLostBeforeWrite
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let blocks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_blocks")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(blocks, 0);
        let state: (String, String) = sqlx::query_as(
            "SELECT state, claim_token
             FROM conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(state, ("delivering".to_string(), "new-owner".to_string()));
    }
}
