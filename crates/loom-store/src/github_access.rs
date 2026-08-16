//! Explicit, revocable GitHub App repository grants layered onto a session's
//! immutable launch policy.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Write,
    None,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::None => "none",
        }
    }
}

impl TryFrom<&str> for Mode {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "write" => Ok(Self::Write),
            "none" => Ok(Self::None),
            _ => anyhow::bail!("unknown GitHub access mode '{value}'"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub repository: String,
    pub mode: Mode,
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
    rows.into_iter()
        .map(|row| {
            let mode = row.get::<String, _>("mode");
            Ok(Grant {
                repository: row.get("repository"),
                mode: Mode::try_from(mode.as_str())?,
                granted_by: row.get("granted_by"),
                granted_at: row.get("granted_at"),
            })
        })
        .collect()
}

pub async fn set(db: &Db, session_id: &str, repository: &str, mode: Mode, by: &str) -> Result<()> {
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
    .bind(mode.as_str())
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

        set(&db, &session, "acme/one", Mode::Write, "alice")
            .await
            .unwrap();
        set(&db, &session, "acme/one", Mode::None, "bob")
            .await
            .unwrap();

        let grants = list(&db, &session).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].repository, "acme/one");
        assert_eq!(grants[0].mode, Mode::None);
        assert_eq!(grants[0].granted_by, "bob");
        assert!(!grants[0].granted_at.is_empty());
    }
}
