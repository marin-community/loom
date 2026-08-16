//! Explicit, revocable GitHub App repository grants layered onto a session's
//! immutable launch policy.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::Db;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub repository: String,
    pub mode: String,
    pub granted_by: String,
    pub granted_at: String,
}

pub async fn list(db: &Db, session_id: &str) -> Result<Vec<Grant>> {
    let rows = sqlx::query(
        "SELECT repository, mode, granted_by, granted_at
         FROM session_github_access WHERE session_id = ? ORDER BY repository",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Grant {
            repository: row.get("repository"),
            mode: row.get("mode"),
            granted_by: row.get("granted_by"),
            granted_at: row.get("granted_at"),
        })
        .collect())
}

pub async fn set(db: &Db, session_id: &str, repository: &str, mode: &str, by: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_github_access
             (session_id, repository, mode, granted_by, granted_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(session_id, repository) DO UPDATE SET
             mode = excluded.mode,
             granted_by = excluded.granted_by,
             granted_at = excluded.granted_at",
    )
    .bind(session_id)
    .bind(repository)
    .bind(mode)
    .bind(by)
    .bind(crate::db::now_iso())
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_session(db: &Db) -> String {
        let branch = weaver_core::branch::upsert(db, "/repo", "weaver/github-access", "main")
            .await
            .unwrap();
        let id = "github-access".to_string();
        crate::session::insert(
            db,
            &crate::session::NewSession {
                id: id.clone(),
                branch_id: branch.id,
                work_dir: "/w".to_string(),
                term_session: "weaver-github-access".to_string(),
                agent_kind: "codex".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some("alice".to_string()),
                protocol: "acp".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn setting_an_override_is_idempotent_and_audited() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let session = seed_session(&db).await;

        set(&db, &session, "acme/one", "write", "alice")
            .await
            .unwrap();
        set(&db, &session, "acme/one", "none", "bob").await.unwrap();

        let grants = list(&db, &session).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].repository, "acme/one");
        assert_eq!(grants[0].mode, "none");
        assert_eq!(grants[0].granted_by, "bob");
        assert!(!grants[0].granted_at.is_empty());
    }
}
