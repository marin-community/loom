//! The `Workspace` model and its database queries.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Workspace {
    pub id: String,
    /// Slug — names the worktree directory and the `weaver/<name>` branch.
    pub name: String,
    /// Human-readable title.
    pub title: String,
    /// What the agent should accomplish; may be empty (agent starts unprompted).
    pub goal: String,
    /// Evolving summary of the workspace's current state.
    pub description: String,
    /// One of: created, launching, working, waiting, idle, orphaned, done, error.
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
    /// While the agent is `waiting`, a snapshot of the prompt it is blocked on;
    /// empty otherwise.
    pub pending_prompt: String,
}

pub const STATUSES: &[&str] = &[
    "created", "launching", "working", "waiting", "idle", "orphaned", "done", "error",
];

/// Whether a status is final. `orphaned` is deliberately non-terminal: a
/// workspace whose tmux session died (e.g. after a reboot) is recoverable and
/// can be adopted back into a running session.
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

/// Derive a human-readable title from goal text: its first non-empty line,
/// trimmed and length-capped.
pub fn derive_title(goal: &str) -> String {
    let first = goal
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.is_empty() {
        return "Untitled workspace".to_string();
    }
    if first.chars().count() > 72 {
        let t: String = first.chars().take(71).collect();
        format!("{t}…")
    } else {
        first.to_string()
    }
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
    pub title: String,
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
    tracing::debug!(id = %w.id, name = %w.name, status = %w.status, "creating workspace");
    sqlx::query(
        "INSERT INTO workspaces
         (id, name, title, goal, description, status, repo_root, work_dir, branch,
          base_branch, tmux_session, agent_kind, github_repo, github_issue)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&w.id)
    .bind(&w.name)
    .bind(&w.title)
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
    let rows = sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces ORDER BY created_at DESC")
        .fetch_all(db)
        .await
        .map_err(|e| {
            // A column-count mismatch here (old on-disk db missing newer
            // columns) surfaces as a row-mapping error — log it so the cause
            // is obvious rather than just a panic deep in sqlx.
            tracing::error!(error = %e, "listing workspaces failed: row mapping error (schema mismatch?)");
            e
        })?;
    tracing::debug!(count = rows.len(), "listed workspaces");
    Ok(rows)
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Workspace>> {
    let row = sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| {
            tracing::error!(%id, error = %e, "getting workspace failed: row mapping error (schema mismatch?)");
            e
        })?;
    tracing::debug!(%id, found = row.is_some(), "got workspace");
    Ok(row)
}

pub async fn find_by_name(db: &Db, name: &str) -> Result<Option<Workspace>> {
    let row = sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces WHERE name = ?")
        .bind(name)
        .fetch_optional(db)
        .await?;
    Ok(row)
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
    let old: Option<String> =
        sqlx::query_scalar("SELECT status FROM workspaces WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .unwrap_or(None);
    sqlx::query("UPDATE workspaces SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(%id, old = old.as_deref().unwrap_or("?"), new = %status, "workspace status changed");
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

pub async fn set_title(db: &Db, id: &str, title: &str) -> Result<()> {
    sqlx::query("UPDATE workspaces SET title = ?, updated_at = ? WHERE id = ?")
        .bind(title)
        .bind(now_iso())
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

/// Store (or, with an empty string, clear) the prompt the agent is waiting on.
pub async fn set_pending_prompt(db: &Db, id: &str, prompt: &str) -> Result<()> {
    tracing::debug!(%id, len = prompt.len(), "set pending prompt");
    sqlx::query("UPDATE workspaces SET pending_prompt = ? WHERE id = ?")
        .bind(prompt)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record a fresh summary: updates the description and the summary timestamp.
pub async fn set_summary(db: &Db, id: &str, description: &str) -> Result<()> {
    tracing::debug!(%id, len = description.len(), "set summary");
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
    tracing::info!(%id, "deleting workspace and its events/summaries");
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
    fn derive_title_uses_first_line() {
        assert_eq!(derive_title("Add a /health endpoint"), "Add a /health endpoint");
        assert_eq!(derive_title("\n\nFix the bug\nmore detail"), "Fix the bug");
        assert_eq!(derive_title("   "), "Untitled workspace");
        assert!(derive_title(&"x".repeat(200)).chars().count() <= 72);
    }

    #[test]
    fn new_id_shape() {
        let id = new_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
