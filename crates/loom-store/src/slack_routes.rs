//! Slack threads an automation delivery pointed at a session.
//!
//! The `slack` branch tag fixes *one* thread as a session's status-card home,
//! which is the right model for a session that was born from a conversation. An
//! operator session is the other shape: it is long-lived, it is fed alerts by
//! [`crate::runs`]'s channel dispatch, and each alert is announced in its own
//! Slack thread. Many threads, one session — so the relation lives here instead,
//! keyed on the thread, because the lookup that matters is inbound: a mention
//! arrives carrying a channel and a thread and has to find the session that owns
//! it.
//!
//! A route is a delivery record, not a grant the caller chooses: it is written
//! only where loom itself accepted a run for that thread, and it is what
//! authorizes a session's later replies into a thread it was not wired to.

use anyhow::Result;
use sqlx::FromRow;

use crate::db::{now_iso, Db};

/// One thread-to-session route.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SlackRoute {
    pub channel_id: String,
    pub thread_ts: String,
    pub branch_id: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Point a Slack thread at a branch. Re-delivering the same alert re-stamps
/// `updated_at`; a thread that somehow reaches a second session moves, because
/// the newest delivery is the one a human replying now means.
pub async fn record(
    db: &Db,
    channel_id: &str,
    thread_ts: &str,
    branch_id: &str,
    source: &str,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO slack_routes
             (channel_id, thread_ts, branch_id, source, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, thread_ts) DO UPDATE SET
             branch_id = excluded.branch_id,
             source = excluded.source,
             updated_at = excluded.updated_at",
    )
    .bind(channel_id)
    .bind(thread_ts)
    .bind(branch_id)
    .bind(source)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(())
}

/// The route for one thread, or `None` when no delivery claimed it.
pub async fn for_thread(db: &Db, channel_id: &str, thread_ts: &str) -> Result<Option<SlackRoute>> {
    let route = sqlx::query_as::<_, SlackRoute>(
        "SELECT channel_id, thread_ts, branch_id, source, created_at, updated_at
         FROM slack_routes WHERE channel_id = ? AND thread_ts = ?",
    )
    .bind(channel_id)
    .bind(thread_ts)
    .fetch_optional(db)
    .await?;
    Ok(route)
}

/// Whether `branch_id` may address this thread — the authorization behind a
/// session replying into a thread it holds no `slack` tag for.
pub async fn allows(db: &Db, branch_id: &str, channel_id: &str, thread_ts: &str) -> Result<bool> {
    let allowed = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM slack_routes
         WHERE branch_id = ? AND channel_id = ? AND thread_ts = ?)",
    )
    .bind(branch_id)
    .bind(channel_id)
    .bind(thread_ts)
    .fetch_one(db)
    .await?;
    Ok(allowed)
}

/// Every thread routed to a branch, most recently delivered first.
pub async fn for_branch(db: &Db, branch_id: &str) -> Result<Vec<SlackRoute>> {
    let routes = sqlx::query_as::<_, SlackRoute>(
        "SELECT channel_id, thread_ts, branch_id, source, created_at, updated_at
         FROM slack_routes WHERE branch_id = ? ORDER BY updated_at DESC",
    )
    .bind(branch_id)
    .fetch_all(db)
    .await?;
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db_with_branch() -> (Db, String) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = weaver_core::branch::upsert(&db, "/tmp/repo", "weaver/ops", "main")
            .await
            .unwrap();
        (db, branch.id)
    }

    #[tokio::test]
    async fn a_recorded_thread_resolves_to_its_branch() {
        let (db, branch) = db_with_branch().await;
        record(&db, "C1", "1700.1", &branch, "grafana")
            .await
            .unwrap();

        let route = for_thread(&db, "C1", "1700.1").await.unwrap().unwrap();
        assert_eq!(route.branch_id, branch);
        assert_eq!(route.source, "grafana");
        assert!(for_thread(&db, "C1", "9999.9").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn one_session_owns_many_threads() {
        let (db, branch) = db_with_branch().await;
        record(&db, "C1", "1700.1", &branch, "grafana")
            .await
            .unwrap();
        record(&db, "C1", "1800.2", &branch, "grafana")
            .await
            .unwrap();

        assert_eq!(for_branch(&db, &branch).await.unwrap().len(), 2);
        assert!(allows(&db, &branch, "C1", "1800.2").await.unwrap());
    }

    /// The route is the authorization: a branch may not address a thread that
    /// was never delivered to it, even in a channel it does hold a route in.
    #[tokio::test]
    async fn a_branch_may_not_address_an_unrouted_thread() {
        let (db, branch) = db_with_branch().await;
        record(&db, "C1", "1700.1", &branch, "grafana")
            .await
            .unwrap();

        assert!(!allows(&db, &branch, "C1", "1700.2").await.unwrap());
        assert!(!allows(&db, &branch, "C2", "1700.1").await.unwrap());
        assert!(!allows(&db, "other-branch", "C1", "1700.1").await.unwrap());
    }

    #[tokio::test]
    async fn redelivery_is_idempotent() {
        let (db, branch) = db_with_branch().await;
        record(&db, "C1", "1700.1", &branch, "grafana")
            .await
            .unwrap();
        record(&db, "C1", "1700.1", &branch, "grafana")
            .await
            .unwrap();

        assert_eq!(for_branch(&db, &branch).await.unwrap().len(), 1);
    }
}
