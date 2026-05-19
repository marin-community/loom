//! The `Workspace` model and its database queries.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub description: String,
    /// One of: created, launching, working, waiting, idle, done, error.
    pub status: String,
    pub repo_root: String,
    pub work_dir: String,
    pub branch: String,
    pub base_branch: String,
    pub tmux_session: String,
    pub agent_kind: String,
    pub github_repo: Option<String>,
    pub github_issue: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub last_activity_at: String,
    pub summary_updated_at: Option<String>,
}

pub const STATUSES: &[&str] = &[
    "created", "launching", "working", "waiting", "idle", "done", "error",
];

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "error")
}

/// An 8-character lowercase-alphanumeric workspace id.
pub fn new_id() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..8)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

/// Turn free text into a short kebab-case slug suitable for a branch name.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "workspace".to_string()
    } else {
        out
    }
}

/// Parameters needed to persist a new workspace row.
pub struct NewWorkspace {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub description: String,
    pub status: String,
    pub repo_root: String,
    pub work_dir: String,
    pub branch: String,
    pub base_branch: String,
    pub tmux_session: String,
    pub agent_kind: String,
    pub github_repo: Option<String>,
    pub github_issue: Option<i64>,
}

pub async fn insert(db: &Db, w: &NewWorkspace) -> Result<Workspace> {
    sqlx::query(
        "INSERT INTO workspaces
         (id, name, goal, description, status, repo_root, work_dir, branch,
          base_branch, tmux_session, agent_kind, github_repo, github_issue)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&w.id)
    .bind(&w.name)
    .bind(&w.goal)
    .bind(&w.description)
    .bind(&w.status)
    .bind(&w.repo_root)
    .bind(&w.work_dir)
    .bind(&w.branch)
    .bind(&w.base_branch)
    .bind(&w.tmux_session)
    .bind(&w.agent_kind)
    .bind(&w.github_repo)
    .bind(w.github_issue)
    .execute(db)
    .await?;
    get(db, &w.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("workspace vanished after insert"))
}

pub async fn list(db: &Db) -> Result<Vec<Workspace>> {
    Ok(
        sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces ORDER BY created_at DESC")
            .fetch_all(db)
            .await?,
    )
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Workspace>> {
    Ok(sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?)
}

/// Resolve a workspace by exact id, exact name, or unambiguous id prefix.
pub async fn resolve(db: &Db, key: &str) -> Result<Option<Workspace>> {
    if let Some(w) = get(db, key).await? {
        return Ok(Some(w));
    }
    let matches = sqlx::query_as::<_, Workspace>(
        "SELECT * FROM workspaces WHERE name = ? OR id LIKE ? ORDER BY created_at DESC",
    )
    .bind(key)
    .bind(format!("{key}%"))
    .fetch_all(db)
    .await?;
    Ok(matches.into_iter().next())
}

pub async fn set_status(db: &Db, id: &str, status: &str) -> Result<()> {
    sqlx::query("UPDATE workspaces SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Bump `last_activity_at` (and `updated_at`).
pub async fn touch(db: &Db, id: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query("UPDATE workspaces SET last_activity_at = ?, updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_goal(db: &Db, id: &str, goal: &str) -> Result<()> {
    sqlx::query("UPDATE workspaces SET goal = ?, updated_at = ? WHERE id = ?")
        .bind(goal)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_description(db: &Db, id: &str, description: &str) -> Result<()> {
    sqlx::query("UPDATE workspaces SET description = ?, updated_at = ? WHERE id = ?")
        .bind(description)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record a fresh summary: updates the description and the summary timestamp.
pub async fn set_summary(db: &Db, id: &str, description: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE workspaces SET description = ?, summary_updated_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(description)
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark the workspace as summarized without changing the description (used
/// when there is no diff to summarize).
pub async fn mark_summarized(db: &Db, id: &str) -> Result<()> {
    sqlx::query("UPDATE workspaces SET summary_updated_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM events WHERE workspace_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM summaries WHERE workspace_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Add a /health endpoint!"), "add-a-health-endpoint");
        assert_eq!(slugify("   "), "workspace");
        assert!(slugify(&"x".repeat(100)).len() <= 32);
    }

    #[test]
    fn new_id_shape() {
        let id = new_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
