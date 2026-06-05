//! Recently-used repositories, stored in the `recent_repos` table.

use anyhow::Result;
use serde::Serialize;
use sqlx::FromRow;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RecentRepo {
    pub repo_root: String,
    pub last_used_at: String,
    /// How many tracked branches exist in this repo (may be zero).
    pub active_branches: i64,
}

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

pub async fn recent(db: &Db, limit: i64) -> Result<Vec<RecentRepo>> {
    let rows = sqlx::query_as::<_, RecentRepo>(
        "SELECT r.repo_root,
                r.last_used_at,
                (SELECT COUNT(*) FROM branches b WHERE b.repo_root = r.repo_root)
                    AS active_branches
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
    use weaver_core::branch;

    #[tokio::test]
    async fn records_and_orders_by_recency() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        record_use(&db, "/code/beta").await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        let rows = recent(&db, 10).await.unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.repo_root.as_str()).collect();
        assert_eq!(paths, ["/code/alpha", "/code/beta"]);
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn counts_tracked_branches() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        branch::upsert(&db, "/code/alpha", "main", "main")
            .await
            .unwrap();
        branch::upsert(&db, "/code/alpha", "feature", "main")
            .await
            .unwrap();
        let rows = recent(&db, 10).await.unwrap();
        assert_eq!(rows[0].active_branches, 2);
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
