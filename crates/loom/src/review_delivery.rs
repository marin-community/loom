//! Delivery worker for submitted reviews.
//!
//! Submission itself only commits durable state. This worker moves the
//! structured feedback across the conversation boundary. ACP delivery first
//! appends to `sessions.pending_prompt` with a stable-key receipt in one
//! transaction, so retrying after a process crash cannot append it twice.
//! Terminal delivery is necessarily at-least-once at the external PTY edge.

use anyhow::{Context, Result};
use serde_json::json;
use std::time::Duration;

use crate::{backend, events, session, AppState};
use weaver_core::review;

const BATCH: i64 = 20;

/// Cross the core-ledger/Loom-runtime seam in one transaction: append the
/// payload to Loom's durable ACP queue, record the stable delivery receipt,
/// and complete the core outbox item. Returns true only for the append owner.
async fn enqueue_prompt_once(
    state: &AppState,
    item: &review::Review,
    session_id: &str,
    payload: &str,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(&state.db).await?;
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM review_prompt_deliveries WHERE delivery_key = ?)",
    )
    .bind(&item.delivery_key)
    .fetch_one(&mut *tx)
    .await?;
    if !already {
        let existing: String =
            sqlx::query_scalar("SELECT pending_prompt FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| anyhow::anyhow!("delivery session not found"))?;
        let combined = if existing.trim().is_empty() {
            payload.to_string()
        } else {
            format!("{existing}\n\n{payload}")
        };
        sqlx::query("UPDATE sessions SET pending_prompt = ? WHERE id = ?")
            .bind(combined)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO review_prompt_deliveries (delivery_key, session_id) VALUES (?, ?)",
        )
        .bind(&item.delivery_key)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivered', attempts = attempts + 1, last_error = NULL
         WHERE review_id = ?",
    )
    .bind(item.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE reviews SET delivery_state = 'delivered', delivery_error = NULL WHERE id = ?",
    )
    .bind(item.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(!already)
}

async fn delivery_session(
    state: &AppState,
    review: &review::Review,
) -> Result<Option<session::Session>> {
    if let Some(target) = session::get(&state.db, &review.session_id).await? {
        if !session::is_terminal(&target.status) {
            return Ok(Some(target));
        }
    }
    session::active_for_branch(&state.db, &review.branch_id).await
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
    if !review::claim_delivery(&state.db, review.id).await? {
        // Another delivery path owns the live lease or already completed it.
        return Ok(());
    }
    let payload = review::structured_message(&review);
    let result = if target.protocol == "acp" {
        let appended = enqueue_prompt_once(state, &review, &target.id, &payload).await?;
        if appended {
            if let Some(handle) = state.acp.get(&target.id) {
                // Best-effort wake. The durable queue is already the delivery
                // boundary; a task race cannot lose or duplicate the review.
                if let Err(error) = handle.notify_pending().await {
                    tracing::debug!(
                        session = %target.id,
                        review = review.id,
                        %error,
                        "review queued; ACP task will pick it up at the next boundary"
                    );
                }
            }
        }
        Ok(())
    } else if !backend::has_session(&target.term_session).await {
        Err(anyhow::anyhow!("target terminal is not running"))
    } else {
        backend::paste(&target.term_session, &payload)
            .await
            .context("pasting review feedback")?;
        backend::send_enter(&target.term_session)
            .await
            .context("submitting review feedback")?;
        review::mark_delivered(&state.db, review.id).await
    };

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
            review::mark_retry(&state.db, review.id, &error.to_string()).await?;
            events::emit(
                &state.bus,
                &review.branch_id,
                "review_delivery",
                json!({
                    "review_id": review.id,
                    "delivery_state": "retrying",
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
