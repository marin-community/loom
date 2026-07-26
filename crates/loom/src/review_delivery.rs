//! Delivery worker for submitted reviews.
//!
//! Submission itself only commits durable state. This worker moves the
//! structured feedback across the conversation boundary. ACP delivery writes a
//! protected immutable inbox lane with a stable-key receipt in one transaction;
//! editable `sessions.pending_prompt` text never contains submitted feedback.
//! Terminal delivery is necessarily at-least-once at the external PTY edge.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{future::Future, time::Duration};

use crate::{backend, events, session, AppState};
use weaver_core::review;

const BATCH: i64 = 20;
const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, sqlx::FromRow)]
pub struct ReviewInboxItem {
    pub delivery_key: String,
    pub payload: String,
    pub claim_token: String,
}

pub struct ReviewTurnBoundary<'a> {
    pub turn: i64,
    pub seq: i64,
    pub opening_payload: &'a Value,
    pub inflight: &'a str,
}

/// Cross the core-ledger/Loom-runtime seam in one transaction: append the
/// payload to Loom's durable ACP queue, record the stable delivery receipt,
/// and complete the core outbox item. Returns true only for the append owner.
async fn enqueue_review_once(
    state: &AppState,
    item: &review::Review,
    session_id: &str,
    payload: &str,
    lease_token: &str,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(&state.db).await?;
    let owns_lease: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM review_delivery_outbox
           WHERE review_id = ? AND state = 'delivering' AND lease_token = ?
         )",
    )
    .bind(item.id)
    .bind(lease_token)
    .fetch_one(&mut *tx)
    .await?;
    if !owns_lease {
        return Err(anyhow::anyhow!(
            "review delivery lease is no longer current"
        ));
    }

    let inserted = sqlx::query(
        "INSERT INTO review_conversation_inbox
            (delivery_key, review_id, branch_id, preferred_session_id, payload)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(delivery_key) DO NOTHING",
    )
    .bind(&item.delivery_key)
    .bind(item.id)
    .bind(&item.branch_id)
    .bind(session_id)
    .bind(payload)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    let completed = sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivered', attempts = attempts + 1, last_error = NULL,
             lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(item.id)
    .bind(lease_token)
    .execute(&mut *tx)
    .await?;
    if completed.rows_affected() == 1 {
        sqlx::query(
            "UPDATE reviews SET delivery_state = 'delivered', delivery_error = NULL
             WHERE id = ? AND delivery_state = 'delivering'",
        )
        .bind(item.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(inserted)
}

async fn usable_session(state: &AppState, target: &session::Session) -> bool {
    if target.status != "running" {
        return false;
    }
    if target.protocol == "acp" {
        state.acp.is_live(&target.id)
    } else {
        tokio::time::timeout(
            TRANSPORT_TIMEOUT,
            backend::has_session(&target.term_session),
        )
        .await
        .unwrap_or(false)
    }
}

async fn delivery_session(
    state: &AppState,
    review: &review::Review,
) -> Result<Option<session::Session>> {
    if let Some(target) = session::get(&state.db, &review.session_id).await? {
        if usable_session(state, &target).await {
            return Ok(Some(target));
        }
    }
    let fallback = session::active_for_branch(&state.db, &review.branch_id).await?;
    match fallback {
        Some(target) if usable_session(state, &target).await => Ok(Some(target)),
        _ => Ok(None),
    }
}

/// Claim the oldest immutable submitted-review message for this branch. The
/// branch address lets a replacement ACP session recover feedback that was
/// originally aimed at an unavailable predecessor.
pub async fn claim_review_inbox(
    db: &crate::db::Db,
    branch_id: &str,
    session_id: &str,
) -> Result<Option<ReviewInboxItem>> {
    let stale = chrono::Utc::now()
        .checked_sub_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(crate::db::now_iso);
    let now = crate::db::now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let key: Option<String> = sqlx::query_scalar(
        "SELECT inbox.delivery_key
         FROM review_conversation_inbox AS inbox
         WHERE inbox.branch_id = ?
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
         ORDER BY inbox.created_at, inbox.delivery_key
         LIMIT 1",
    )
    .bind(branch_id)
    .bind(stale)
    .fetch_optional(&mut *tx)
    .await?;
    let item = match key {
        Some(key) => {
            sqlx::query_as::<_, ReviewInboxItem>(
                "UPDATE review_conversation_inbox
                 SET state = 'delivering',
                     claimed_session_id = ?,
                     claim_token = lower(hex(randomblob(16))),
                     claimed_at = ?
                 WHERE delivery_key = ?
                 RETURNING delivery_key, payload, claim_token",
            )
            .bind(session_id)
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

/// Hand protected feedback to the ACP socket, then atomically persist its
/// opening journal block and in-flight prompt. The socket has no durable
/// supervisor acknowledgement, so the inbox claim remains recoverable until
/// the matching ACP prompt response consumes it. A failure can retry the
/// immutable feedback and may duplicate process delivery, but cannot lose it.
pub async fn start_review_inbox_turn<F>(
    db: &crate::db::Db,
    item: &ReviewInboxItem,
    session_id: &str,
    boundary: ReviewTurnBoundary<'_>,
    transport: F,
) -> Result<bool>
where
    F: Future<Output = Result<()>>,
{
    let owns_claim: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM review_conversation_inbox
           WHERE delivery_key = ? AND state = 'delivering'
             AND claim_token = ? AND claimed_session_id = ?
         )",
    )
    .bind(&item.delivery_key)
    .bind(&item.claim_token)
    .bind(session_id)
    .fetch_one(db)
    .await?;
    if !owns_claim {
        return Ok(false);
    }

    transport
        .await
        .context("writing protected review feedback to ACP relay")?;

    let now = crate::db::now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let still_owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM review_conversation_inbox
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
        return Ok(false);
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
        return Err(anyhow::anyhow!(
            "protected review turn journal position is already occupied"
        ));
    }
    let session = sqlx::query("UPDATE sessions SET acp_inflight = ? WHERE id = ?")
        .bind(boundary.inflight)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    if session.rows_affected() != 1 {
        return Err(anyhow::anyhow!(
            "protected review target session disappeared"
        ));
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn release_review_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
) -> Result<bool> {
    let released = sqlx::query(
        "UPDATE review_conversation_inbox
         SET state = 'queued', claim_token = NULL, claimed_at = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(delivery_key)
    .bind(claim_token)
    .execute(db)
    .await?;
    Ok(released.rows_affected() == 1)
}

pub async fn consume_review_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
    session_id: &str,
) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE review_conversation_inbox
         SET state = 'consumed', consumed_at = ?, claimed_session_id = ?,
             claim_token = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(crate::db::now_iso())
    .bind(session_id)
    .bind(delivery_key)
    .bind(claim_token)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

pub async fn deliver_review(state: &AppState, review_id: i64) -> Result<()> {
    let Some(review) = review::get(&state.db, review_id).await? else {
        return Ok(());
    };
    if review.status != "submitted"
        || !matches!(
            review.delivery_state.as_str(),
            "queued" | "retrying" | "delivering" | "failed"
        )
    {
        return Ok(());
    }
    let Some(target) = delivery_session(state, &review).await? else {
        // Honest offline state: keep the outbox queued until this branch has a
        // conversation again. An expired prior owner must not leave the public
        // state stuck at `delivering`.
        review::release_expired_delivery(&state.db, review.id).await?;
        return Ok(());
    };
    let Some(lease) = review::claim_delivery(&state.db, review.id).await? else {
        // Another delivery path owns the live lease or already completed it.
        return Ok(());
    };
    let payload = review::structured_message(&review);
    let result: Result<()> = async {
        if target.protocol == "acp" {
            let appended =
                enqueue_review_once(state, &review, &target.id, &payload, &lease.token).await?;
            if appended {
                if let Some(handle) = state.acp.get(&target.id) {
                    // Best-effort wake. The durable queue survives task races;
                    // the unacknowledged ACP process boundary may retry.
                    match tokio::time::timeout(TRANSPORT_TIMEOUT, handle.notify_pending()).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            tracing::debug!(
                                session = %target.id,
                                review = review.id,
                                %error,
                                "review queued; ACP task will pick it up at the next boundary"
                            );
                        }
                        Err(_) => {
                            tracing::debug!(
                                session = %target.id,
                                review = review.id,
                                "review queued; ACP wake timed out and will be retried by the sweep"
                            );
                        }
                    }
                }
            }
            Ok(())
        } else if !tokio::time::timeout(
            TRANSPORT_TIMEOUT,
            backend::has_session(&target.term_session),
        )
        .await
        .unwrap_or(false)
        {
            // It disappeared after the pre-claim liveness check. This is a real
            // attempted transport, so let the fenced retry path record it.
            Err(anyhow::anyhow!(
                "target terminal disappeared during delivery"
            ))
        } else {
            tokio::time::timeout(TRANSPORT_TIMEOUT, async {
                backend::paste(&target.term_session, &payload)
                    .await
                    .context("pasting review feedback")?;
                backend::send_enter(&target.term_session)
                    .await
                    .context("submitting review feedback")
            })
            .await
            .map_err(|_| anyhow::anyhow!("review delivery transport timed out"))??;
            if !review::mark_delivered(&state.db, review.id, &lease.token).await? {
                return Err(anyhow::anyhow!(
                    "review delivery lease is no longer current"
                ));
            }
            Ok(())
        }
    }
    .await;

    match result {
        Ok(()) => {
            events::emit(
                &state.bus,
                &review.branch_id,
                "review_delivery",
                json!({
                    "review_id": review.id,
                    "delivery_state": "delivered",
                    "session_id": review.session_id,
                    "delivery_session_id": target.id,
                    "subject_key": review.subject_key,
                }),
            );
            Ok(())
        }
        Err(error) => {
            let Some(delivery_state) =
                review::mark_retry(&state.db, review.id, &lease.token, &error.to_string()).await?
            else {
                // The lease expired and a newer owner already decided the
                // state. The stale worker is fenced from regressing it.
                return Ok(());
            };
            events::emit(
                &state.bus,
                &review.branch_id,
                "review_delivery",
                json!({
                    "review_id": review.id,
                    "delivery_state": delivery_state,
                    "session_id": review.session_id,
                    "delivery_session_id": target.id,
                    "subject_key": review.subject_key,
                    "error": error.to_string(),
                }),
            );
            Err(error)
        }
    }
}

pub async fn drain(state: &AppState) -> Result<()> {
    drain_with_timeout(state, TRANSPORT_TIMEOUT).await
}

async fn drain_with_timeout(state: &AppState, transport_timeout: Duration) -> Result<()> {
    for item in review::ready_outbox(&state.db, BATCH).await? {
        if let Err(error) = deliver_review(state, item.review_id).await {
            tracing::warn!(
                review = item.review_id,
                attempts = item.attempts + 1,
                %error,
                "review delivery attempt failed"
            );
        }
    }
    let branches: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT branch_id FROM review_conversation_inbox
         WHERE state IN ('queued', 'delivering')
         ORDER BY branch_id",
    )
    .fetch_all(&state.db)
    .await?;
    for branch_id in branches {
        if let Some(target) = session::active_for_branch(&state.db, &branch_id).await? {
            if !usable_session(state, &target).await {
                continue;
            }
            if target.protocol == "acp" {
                if let Some(handle) = state.acp.get(&target.id) {
                    // A wedged ACP task must not pin the single delivery sweep
                    // and starve every later branch.
                    let _ = tokio::time::timeout(transport_timeout, handle.notify_pending()).await;
                }
                continue;
            }
            let Some(item) = claim_review_inbox(&state.db, &branch_id, &target.id).await? else {
                continue;
            };
            let sent = tokio::time::timeout(transport_timeout, async {
                backend::paste(&target.term_session, &item.payload)
                    .await
                    .context("pasting protected review feedback")?;
                backend::send_enter(&target.term_session)
                    .await
                    .context("submitting protected review feedback")
            })
            .await
            .map_err(|_| anyhow::anyhow!("protected review transport timed out"))
            .and_then(|result| result);
            match sent {
                Ok(()) => {
                    if !consume_review_inbox(
                        &state.db,
                        &item.delivery_key,
                        &item.claim_token,
                        &target.id,
                    )
                    .await?
                    {
                        tracing::warn!(
                            delivery_key = %item.delivery_key,
                            session = %target.id,
                            "protected review transport completed after its inbox claim was lost"
                        );
                    }
                }
                Err(error) => {
                    let _ = release_review_inbox(&state.db, &item.delivery_key, &item.claim_token)
                        .await;
                    tracing::warn!(
                        delivery_key = %item.delivery_key,
                        session = %target.id,
                        %error,
                        "protected review terminal rehome failed"
                    );
                }
            }
        }
    }
    Ok(())
}

pub async fn run(state: AppState) {
    loop {
        if let Err(error) = drain(&state).await {
            tracing::warn!(%error, "review delivery sweep failed");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn claimed_inbox() -> (crate::db::Db, ReviewInboxItem) {
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
            "INSERT INTO review_conversation_inbox
                (delivery_key, review_id, branch_id, preferred_session_id, payload)
             VALUES ('review:stable', 1, ?, 'acp-review', 'immutable')",
        )
        .bind(&branch.id)
        .execute(&db)
        .await
        .unwrap();
        let item = claim_review_inbox(&db, &branch.id, "acp-review")
            .await
            .unwrap()
            .unwrap();
        (db, item)
    }

    #[tokio::test]
    async fn turn_journal_and_claim_stay_recoverable_until_prompt_response() {
        let (db, item) = claimed_inbox().await;
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
        let error = start_review_inbox_turn(
            &db,
            &item,
            "acp-review",
            ReviewTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &payload,
                inflight: r#"{"prompt_id":7,"turn":1,"delivery_key":"review:stable"}"#,
            },
            async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected failure"));
        let state: String = sqlx::query_scalar(
            "SELECT state FROM review_conversation_inbox WHERE delivery_key = 'review:stable'",
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

        sqlx::query("DROP TRIGGER fail_review_inflight")
            .execute(&db)
            .await
            .unwrap();
        assert!(start_review_inbox_turn(
            &db,
            &item,
            "acp-review",
            ReviewTurnBoundary {
                turn: 1,
                seq: 0,
                opening_payload: &payload,
                inflight: r#"{"prompt_id":7,"turn":1,"delivery_key":"review:stable"}"#,
            },
            async { Ok(()) },
        )
        .await
        .unwrap());
        let state: String = sqlx::query_scalar(
            "SELECT state FROM review_conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            state, "delivering",
            "a socket write is not an ACP prompt acknowledgement"
        );
        assert!(
            consume_review_inbox(&db, &item.delivery_key, &item.claim_token, "acp-review")
                .await
                .unwrap()
        );
        assert!(
            !start_review_inbox_turn(
                &db,
                &item,
                "acp-review",
                ReviewTurnBoundary {
                    turn: 2,
                    seq: 0,
                    opening_payload: &payload,
                    inflight: r#"{"prompt_id":8,"turn":2,"delivery_key":"review:stable"}"#,
                },
                async { Ok(()) },
            )
            .await
            .unwrap(),
            "a consumed or lost claim is never accepted as another turn"
        );
        let row: (String, i64, String) = sqlx::query_as(
            "SELECT i.state, COUNT(b.id), b.payload
             FROM review_conversation_inbox i
             JOIN chat_blocks b
               ON json_extract(b.payload, '$.delivery_key') = i.delivery_key
             WHERE i.delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row.0, "consumed");
        assert_eq!(row.1, 1);
        assert_eq!(
            serde_json::from_str::<Value>(&row.2).unwrap()["delivery_key"],
            "review:stable"
        );
    }

    #[tokio::test]
    async fn relay_write_failure_keeps_protected_feedback_recoverable() {
        let (db, item) = claimed_inbox().await;
        let error = start_review_inbox_turn(
            &db,
            &item,
            "acp-review",
            ReviewTurnBoundary {
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
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected relay write failure"));

        let state: String = sqlx::query_scalar(
            "SELECT state FROM review_conversation_inbox WHERE delivery_key = 'review:stable'",
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
            release_review_inbox(&db, &item.delivery_key, &item.claim_token)
                .await
                .unwrap()
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM review_conversation_inbox WHERE delivery_key = 'review:stable'",
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
            "UPDATE review_conversation_inbox
             SET claim_token = 'new-owner' WHERE delivery_key = ?",
        )
        .bind(&item.delivery_key)
        .execute(&db)
        .await
        .unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let observed_writes = writes.clone();
        let started = start_review_inbox_turn(
            &db,
            &item,
            "acp-review",
            ReviewTurnBoundary {
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
        .await
        .unwrap();
        assert!(!started);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let blocks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_blocks")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(blocks, 0);
        let state: (String, String) = sqlx::query_as(
            "SELECT state, claim_token
             FROM review_conversation_inbox WHERE delivery_key = 'review:stable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(state, ("delivering".to_string(), "new-owner".to_string()));
    }

    #[tokio::test]
    async fn wedged_acp_wake_does_not_block_later_branches() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut branches = vec![
            weaver_core::branch::upsert(&db, "/repo", "a-wedged", "main")
                .await
                .unwrap(),
            weaver_core::branch::upsert(&db, "/repo", "b-ready", "main")
                .await
                .unwrap(),
        ];
        branches.sort_by(|left, right| left.id.cmp(&right.id));
        for (index, branch) in branches.into_iter().enumerate() {
            let (session_id, key) = if index == 0 {
                ("acp-wedged", "review:wedged")
            } else {
                ("acp-ready", "review:ready")
            };
            let review_id = index as i64 + 1;
            sqlx::query(
                "INSERT INTO sessions
                    (id, branch_id, work_dir, term_session, status, protocol)
                 VALUES (?, ?, '/repo', ?, 'running', 'acp')",
            )
            .bind(session_id)
            .bind(&branch.id)
            .bind(session_id)
            .execute(&db)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO reviews
                    (id, repo_root, branch_id, session_id, subject_kind, subject_id,
                     subject_key, subject_label, subject_version, status, created_by,
                     delivery_state, delivery_key)
                 VALUES (?, '/repo', ?, ?, 'artifact', ?, 'design', 'design', '1',
                         'submitted', 'alice', 'delivered', ?)",
            )
            .bind(review_id)
            .bind(&branch.id)
            .bind(session_id)
            .bind(review_id.to_string())
            .bind(key)
            .execute(&db)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO review_conversation_inbox
                    (delivery_key, review_id, branch_id, preferred_session_id, payload)
                 VALUES (?, ?, ?, ?, 'immutable')",
            )
            .bind(key)
            .bind(review_id)
            .bind(&branch.id)
            .bind(session_id)
            .execute(&db)
            .await
            .unwrap();
        }
        let state = AppState {
            db: db.clone(),
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        };
        let wedged_wakes = Arc::new(AtomicUsize::new(0));
        let ready_wakes = Arc::new(AtomicUsize::new(0));
        state
            .acp
            .register_review_wake_probe("acp-wedged", false, Arc::clone(&wedged_wakes));
        state
            .acp
            .register_review_wake_probe("acp-ready", true, Arc::clone(&ready_wakes));

        drain_with_timeout(&state, Duration::from_millis(10))
            .await
            .unwrap();

        assert_eq!(wedged_wakes.load(Ordering::SeqCst), 1);
        assert_eq!(
            ready_wakes.load(Ordering::SeqCst),
            1,
            "the sweep must continue after the bounded wake times out"
        );
    }
}
