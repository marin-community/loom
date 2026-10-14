//! Protected conversation input shared by reviews and session updates.
//!
//! An immutable update becomes an inbox item that a session's ACP task claims,
//! opens a turn against, and then settles. The claim is a durable lease: the row
//! carries the token, so a task that dies mid-turn releases its claim on restart
//! rather than wedging the lane. Session updates stay addressed to their exact
//! recipient; reviews may follow a replacement session on the branch.

use anyhow::{Context, Result};
use serde_json::Value;
use std::future::Future;

#[derive(Debug, sqlx::FromRow)]
pub struct InboxItem {
    pub delivery_key: String,
    pub payload: String,
    pub claim_token: String,
    pub source_session_id: Option<String>,
    pub source_channel_id: Option<String>,
    pub source_message_id: Option<String>,
}

pub async fn pending_updates(
    db: &crate::db::Db,
    session: &str,
) -> Result<Vec<weaver_api::SessionUpdateView>> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT delivery_key, source_session_id, source_channel_id, source_message_id,
                payload, state FROM conversation_inbox
         WHERE preferred_session_id = ? AND review_id IS NULL AND state = 'queued'
         ORDER BY created_at, rowid",
    )
    .bind(session)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(weaver_api::SessionUpdateView {
                delivery_key: row.try_get("delivery_key")?,
                source_session_id: row.try_get("source_session_id")?,
                source_channel_id: row.try_get("source_channel_id")?,
                source_message_id: row.try_get("source_message_id")?,
                text: row.try_get("payload")?,
                state: row.try_get("state")?,
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
enum InboxLane {
    Review,
    Session,
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
    let payload = format!("Update from loom session {source}\n\n{text}");
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
        "INSERT INTO conversation_inbox (delivery_key, branch_id, preferred_session_id, payload, source_session_id)
         SELECT ?, branch_id, id, ?, ? FROM sessions WHERE id = ? AND status != 'archived'
         ON CONFLICT(delivery_key) DO NOTHING",
    ).bind(&key).bind(payload).bind(source).bind(session).execute(&mut *tx).await?;
    if existing.is_none() && inserted.rows_affected() == 0 {
        anyhow::bail!("session is missing or archived");
    }
    tx.commit().await?;
    Ok(key)
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

pub async fn claim_session_update(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
    claim_owner: Option<&str>,
    owner_is_live: impl Fn(&str, &str) -> bool,
) -> Result<Option<InboxItem>> {
    claim_at(
        db,
        branch_id,
        session_id,
        claim_owner,
        owner_is_live,
        chrono::Utc::now(),
        InboxLane::Session,
    )
    .await
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
    lane: InboxLane,
) -> Result<Option<InboxItem>> {
    let stale = now
        .checked_sub_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(crate::db::now_iso);
    let now = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let candidates: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT inbox.delivery_key, inbox.state,
                inbox.claimed_session_id, inbox.claim_owner
         FROM conversation_inbox AS inbox
         WHERE inbox.branch_id = ?
           AND CASE WHEN ? THEN inbox.review_id IS NOT NULL
                    ELSE inbox.review_id IS NULL AND inbox.preferred_session_id = ? END
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
                 RETURNING delivery_key, payload, claim_token,
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
    let session = sqlx::query("UPDATE sessions SET acp_inflight = ? WHERE id = ?")
        .bind(boundary.inflight)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    if session.rows_affected() != 1 {
        anyhow::bail!("protected inbox target session disappeared");
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
        let pending = pending_updates(&db, "acp-review").await.unwrap();
        assert_eq!(
            pending.len(),
            2,
            "distinct publications may have identical text"
        );
        assert_eq!(pending[0].source_session_id.as_deref(), Some("child"));
        assert!(crate::session::take_pending_prompt(&db, "acp-review")
            .await
            .unwrap()
            .is_none());
        assert_eq!(pending_updates(&db, "acp-review").await.unwrap().len(), 2);
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
        assert!(
            claim_session_update(&db, &branch, "other-session", Some("other"), |_, _| false)
                .await
                .unwrap()
                .is_none()
        );
        let item = claim_session_update(&db, &branch, "acp-review", Some("driver"), |_, _| false)
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
        assert!(
            claim_session_update(&db, &branch, "acp-review", Some("replacement"), |_, _| {
                false
            })
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn post_write_persistence_failure_holds_every_followup_until_prompt_response() {
        let (db, item) = claimed_inbox().await;
        sqlx::query(
            "UPDATE sessions SET pending_prompt = 'ordinary queued prompt'
             WHERE id = 'acp-review'",
        )
        .execute(&db)
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
            crate::session::take_pending_prompt(&db, "acp-review")
                .await
                .unwrap()
        };
        assert!(
            ordinary_started.is_none(),
            "the dispatch caller must not drain an ordinary prompt after the relay write"
        );
        assert_eq!(
            crate::session::read_pending_prompt(&db, "acp-review")
                .await
                .unwrap(),
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
            crate::session::take_pending_prompt(&db, "acp-review")
                .await
                .unwrap()
                .as_deref(),
            Some("ordinary queued prompt"),
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
