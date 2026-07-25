//! Delivery worker for submitted reviews.
//!
//! Submission itself only commits durable state. This worker moves the
//! structured feedback across the conversation boundary. ACP delivery writes a
//! protected immutable inbox lane with a stable-key receipt in one transaction;
//! editable `sessions.pending_prompt` text never contains submitted feedback.
//! Terminal delivery is necessarily at-least-once at the external PTY edge.

use anyhow::{Context, Result};
use serde_json::json;
use std::time::Duration;

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
    sqlx::query(
        "INSERT INTO review_prompt_deliveries (delivery_key, session_id)
         VALUES (?, ?) ON CONFLICT(delivery_key) DO NOTHING",
    )
    .bind(&item.delivery_key)
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
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
        "SELECT delivery_key
         FROM review_conversation_inbox
         WHERE branch_id = ?
           AND (state = 'queued' OR (state = 'delivering' AND claimed_at <= ?))
         ORDER BY created_at, delivery_key
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

pub async fn complete_review_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE review_conversation_inbox
         SET state = 'consumed', consumed_at = ?, claim_token = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(crate::db::now_iso())
    .bind(delivery_key)
    .bind(claim_token)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

pub async fn release_review_inbox(
    db: &crate::db::Db,
    delivery_key: &str,
    claim_token: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE review_conversation_inbox
         SET state = 'queued', claim_token = NULL, claimed_at = NULL
         WHERE delivery_key = ? AND state = 'delivering' AND claim_token = ?",
    )
    .bind(delivery_key)
    .bind(claim_token)
    .execute(db)
    .await?;
    Ok(())
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
        // conversation again.
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
                    // Best-effort wake. The durable queue is already the delivery
                    // boundary; a task race cannot lose or duplicate the review.
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
         WHERE state IN ('queued', 'delivering')",
    )
    .fetch_all(&state.db)
    .await?;
    for branch_id in branches {
        if let Some(target) = session::active_for_branch(&state.db, &branch_id).await? {
            if target.status == "running" && target.protocol == "acp" {
                if let Some(handle) = state.acp.get(&target.id) {
                    let _ = handle.notify_pending().await;
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
