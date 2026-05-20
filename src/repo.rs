//! Recently-used repositories, stored in the `recent_repos` table.
//!
//! Every time a workspace is created its repo root is recorded here. Unlike
//! the `workspaces` table, an entry survives the removal of all of a repo's
//! workspaces — that is the point: the dashboard remembers where you have been
//! working so it can offer those repos when you start a new session.

use anyhow::Result;
use serde::Serialize;
use sqlx::FromRow;

use crate::db::{now_iso, Db};

/// A repository the user has started a workspace in.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RecentRepo {
    /// Absolute path to the repository root.
    pub repo_root: String,
    /// When a workspace was last created in this repo.
    pub last_used_at: String,
    /// How many of this repo's workspaces are still tracked (may be zero).
    pub active_workspaces: i64,
}

/// Record that a workspace was just created in `repo_root`, bumping its
/// recency. Inserts the repo on first use and refreshes `last_used_at` after.
pub async fn record_use(db: &Db, repo_root: &str) -> Result<()> {
    tracing::debug!(repo_root, "recording recent repo use");
    sqlx::query(
        "INSERT INTO recent_repos (repo_root, last_used_at) VALUES (?, ?)
         ON CONFLICT(repo_root) DO UPDATE SET last_used_at = excluded.last_used_at",
    )
    .bind(repo_root)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// The most recently used repositories, newest first, capped at `limit`.
pub async fn recent(db: &Db, limit: i64) -> Result<Vec<RecentRepo>> {
    let rows = sqlx::query_as::<_, RecentRepo>(
        "SELECT r.repo_root,
                r.last_used_at,
                (SELECT COUNT(*) FROM workspaces w WHERE w.repo_root = r.repo_root)
                    AS active_workspaces
         FROM recent_repos r
         ORDER BY r.last_used_at DESC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    tracing::debug!(count = rows.len(), "listed recent repos");
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_in_memory;

    async fn add_workspace(db: &Db, id: &str, repo_root: &str) {
        sqlx::query(
            "INSERT INTO workspaces (id,name,repo_root,work_dir,branch,tmux_session)
             VALUES (?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(id)
        .bind(repo_root)
        .bind(format!("/w/{id}"))
        .bind(format!("b/{id}"))
        .bind(format!("s-{id}"))
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn records_and_orders_by_recency() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        record_use(&db, "/code/beta").await.unwrap();
        // Re-using alpha moves it back to the front.
        record_use(&db, "/code/alpha").await.unwrap();

        let rows = recent(&db, 10).await.unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.repo_root.as_str()).collect();
        assert_eq!(paths, ["/code/alpha", "/code/beta"]);
        assert_eq!(rows.len(), 2, "re-use must not create a duplicate row");
    }

    #[tokio::test]
    async fn counts_active_workspaces_and_survives_their_removal() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        add_workspace(&db, "w1", "/code/alpha").await;
        add_workspace(&db, "w2", "/code/alpha").await;

        let rows = recent(&db, 10).await.unwrap();
        assert_eq!(rows[0].active_workspaces, 2);

        // Removing every workspace leaves the repo in the list, count zero.
        sqlx::query("DELETE FROM workspaces").execute(&db).await.unwrap();
        let rows = recent(&db, 10).await.unwrap();
        assert_eq!(rows.len(), 1, "repo is remembered after its workspaces go");
        assert_eq!(rows[0].active_workspaces, 0);
    }

    #[tokio::test]
    async fn limit_caps_the_result() {
        let db = connect_in_memory().await.unwrap();
        for i in 0..5 {
            record_use(&db, &format!("/code/r{i}")).await.unwrap();
        }
        assert_eq!(recent(&db, 3).await.unwrap().len(), 3);
    }
}
