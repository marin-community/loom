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
