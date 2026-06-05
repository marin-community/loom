//! The `Branch` model: a `(repo_root, branch)` pair the agent is working on,
//! plus the helpers used to resolve "the current branch" from the environment.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::path::{Path, PathBuf};

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Branch {
    pub id: String,
    pub repo_root: String,
    pub branch: String,
    pub base_branch: String,
    pub goal: String,
    pub title: String,
    pub description: String,
    /// Agent-declared attention level: one of [`ATTENTION_LEVELS`].
    pub attention: String,
    /// Short free-text reason for the current attention level.
    pub attention_note: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Agent-declared attention levels, ordered calm → urgent:
///
/// * `ok` — progressing fine, or blocked on something external (a CI run, a PR
///   review) that is *not* the user. No action needed.
/// * `attention` — the agent wants the user to look: a question, a decision to
///   confirm, or "done, ready for review".
/// * `blocked` — the agent is stuck or hit an error and needs help to proceed.
///
/// This is the agent's own signal, distinct from the orchestrator's mechanical
/// session lifecycle (`launching` / `running` / `orphaned` / …). It is what the
/// dashboard surfaces and filters on for "which sessions need me?".
pub const ATTENTION_LEVELS: &[&str] = &["ok", "attention", "blocked"];

/// The default attention level for a freshly-created branch.
pub const DEFAULT_ATTENTION: &str = "ok";

/// Whether `level` is a recognized attention level.
pub fn is_valid_attention(level: &str) -> bool {
    ATTENTION_LEVELS.contains(&level)
}

/// An 8-character lowercase-alphanumeric id (re-used for branches and sessions).
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
        return "Untitled".to_string();
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
        "branch".to_string()
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

pub async fn get(db: &Db, id: &str) -> Result<Option<Branch>> {
    let row = sqlx::query_as::<_, Branch>("SELECT * FROM branches WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

pub async fn find_by_repo_branch(
    db: &Db,
    repo_root: &str,
    branch: &str,
) -> Result<Option<Branch>> {
    let row = sqlx::query_as::<_, Branch>(
        "SELECT * FROM branches WHERE repo_root = ? AND branch = ?",
    )
    .bind(repo_root)
    .bind(branch)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

pub async fn list(db: &Db) -> Result<Vec<Branch>> {
    let rows = sqlx::query_as::<_, Branch>("SELECT * FROM branches ORDER BY created_at DESC")
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Look up a branch by id, exact `repo:branch` spec, or unambiguous id prefix.
pub async fn resolve_key(db: &Db, key: &str) -> Result<Option<Branch>> {
    if let Some(b) = get(db, key).await? {
        return Ok(Some(b));
    }
    if let Some((repo, branch)) = key.split_once(':') {
        if let Some(b) = find_by_repo_branch(db, repo, branch).await? {
            return Ok(Some(b));
        }
    }
    let matches = sqlx::query_as::<_, Branch>(
        "SELECT * FROM branches WHERE branch = ? OR id LIKE ? ORDER BY created_at DESC",
    )
    .bind(key)
    .bind(format!("{key}%"))
    .fetch_all(db)
    .await?;
    Ok(matches.into_iter().next())
}

/// Create a new branch row.
pub async fn insert(
    db: &Db,
    id: &str,
    repo_root: &str,
    branch: &str,
    base_branch: &str,
) -> Result<Branch> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO branches (id, repo_root, branch, base_branch, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(repo_root)
    .bind(branch)
    .bind(base_branch)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    get(db, id)
        .await?
        .ok_or_else(|| anyhow!("branch vanished after insert"))
}

/// Get or create a branch by `(repo_root, branch)`.
pub async fn upsert(
    db: &Db,
    repo_root: &str,
    branch: &str,
    base_branch: &str,
) -> Result<Branch> {
    if let Some(b) = find_by_repo_branch(db, repo_root, branch).await? {
        return Ok(b);
    }
    let id = new_id();
    insert(db, &id, repo_root, branch, base_branch).await
}

pub async fn set_goal(db: &Db, id: &str, goal: &str) -> Result<()> {
    sqlx::query("UPDATE branches SET goal = ?, updated_at = ? WHERE id = ?")
        .bind(goal)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_title(db: &Db, id: &str, title: &str) -> Result<()> {
    sqlx::query("UPDATE branches SET title = ?, updated_at = ? WHERE id = ?")
        .bind(title)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_description(db: &Db, id: &str, description: &str) -> Result<()> {
    sqlx::query("UPDATE branches SET description = ?, updated_at = ? WHERE id = ?")
        .bind(description)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Set the agent-declared attention level and its short note in one write.
/// `level` is assumed already validated by the caller (see [`is_valid_attention`]).
pub async fn set_attention(db: &Db, id: &str, level: &str, note: &str) -> Result<()> {
    sqlx::query(
        "UPDATE branches SET attention = ?, attention_note = ?, updated_at = ? WHERE id = ?",
    )
    .bind(level)
    .bind(note)
    .bind(now_iso())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM branches WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Branch resolution from environment + cwd
// ---------------------------------------------------------------------------

/// Walk up from `dir` looking for a `.git` entry (either a directory or a file
/// that points at a worktree's git dir). Returns the directory containing it.
fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    loop {
        let candidate = cur.join(".git");
        if candidate.exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Read the current branch by parsing `<git_dir>/HEAD`.
///
/// HEAD is the simplest thing that works without spawning git, which keeps cold
/// start fast. A detached HEAD yields `None` (the agent should run inside an
/// attached branch).
fn read_head_branch(repo_root: &Path) -> Option<String> {
    // For a primary worktree, HEAD lives at .git/HEAD. For a linked worktree,
    // .git is a file pointing at `gitdir: <path>` and HEAD is at <path>/HEAD.
    let dot_git = repo_root.join(".git");
    let head_path = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else if let Ok(contents) = std::fs::read_to_string(&dot_git) {
        let trimmed = contents.trim();
        let gitdir = trimmed.strip_prefix("gitdir:")?.trim();
        PathBuf::from(gitdir).join("HEAD")
    } else {
        return None;
    };
    let head = std::fs::read_to_string(head_path).ok()?;
    let line = head.trim();
    line.strip_prefix("ref: refs/heads/").map(str::to_string)
}

/// Find the **primary** worktree (the repo's main checkout) from a worktree's
/// `.git` file. Returns `repo_root` itself when called on the primary tree.
fn primary_worktree(dir: &Path) -> PathBuf {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return dir.to_path_buf();
    }
    // Linked worktree: parse `gitdir: <abs path to .git/worktrees/<name>>`.
    if let Ok(contents) = std::fs::read_to_string(&dot_git) {
        if let Some(gitdir) = contents
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .map(PathBuf::from)
        {
            // The primary repo's `.git` dir is the parent of `worktrees/<name>`.
            // e.g. /repo/.git/worktrees/foo  → /repo/.git → /repo
            let primary_git = gitdir.parent().and_then(Path::parent);
            if let Some(g) = primary_git {
                if let Some(p) = g.parent() {
                    return p.to_path_buf();
                }
            }
        }
    }
    dir.to_path_buf()
}

/// Resolve the branch the current process is operating on. Order:
///   1. `$WEAVER_BRANCH` (an internal branch id, set by `loom launch`)
///   2. Walk up from `cwd` to find a git checkout, read `HEAD`, and look up
///      `(repo_root, branch)` in the `branches` table. Auto-creates the row
///      on first call.
pub async fn resolve(db: &Db) -> Result<Branch> {
    if let Ok(id) = std::env::var("WEAVER_BRANCH") {
        if !id.is_empty() {
            if let Some(b) = get(db, &id).await? {
                return Ok(b);
            }
        }
    }
    let cwd = std::env::current_dir().context("getting cwd")?;
    resolve_from_path(db, &cwd).await
}

/// Resolve (and, on first write, create) the branch row for the git checkout
/// containing `dir`. Errors when `dir` is not inside a git repo or when HEAD is
/// detached.
pub async fn resolve_from_path(db: &Db, dir: &Path) -> Result<Branch> {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let worktree = find_git_dir(&canonical).ok_or_else(|| {
        anyhow!(
            "not inside a git repository (and $WEAVER_BRANCH is not set): {}",
            canonical.display()
        )
    })?;
    let branch = read_head_branch(&worktree).ok_or_else(|| {
        anyhow!(
            "could not determine branch from HEAD (detached?): {}",
            worktree.display()
        )
    })?;
    let repo_root = primary_worktree(&worktree);
    let repo_root_str = repo_root.canonicalize().unwrap_or(repo_root.clone());
    upsert(db, &repo_root_str.display().to_string(), &branch, "main").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Add a /health endpoint!"), "add-a-health-endpoint");
        assert_eq!(slugify("   "), "branch");
        assert!(slugify(&"x".repeat(100)).len() <= 32);
    }

    #[test]
    fn derive_title_uses_first_line() {
        assert_eq!(derive_title("Add a /health endpoint"), "Add a /health endpoint");
        assert_eq!(derive_title("\n\nFix the bug\nmore detail"), "Fix the bug");
        assert_eq!(derive_title("   "), "Untitled");
        assert!(derive_title(&"x".repeat(200)).chars().count() <= 72);
    }

    #[test]
    fn new_id_shape() {
        let id = new_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[tokio::test]
    async fn upsert_is_idempotent() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b1 = upsert(&db, "/r", "main", "main").await.unwrap();
        let b2 = upsert(&db, "/r", "main", "main").await.unwrap();
        assert_eq!(b1.id, b2.id);
    }

    #[tokio::test]
    async fn resolve_uses_env_branch_id() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = upsert(&db, "/r", "main", "main").await.unwrap();
        std::env::set_var("WEAVER_BRANCH", &b.id);
        let resolved = resolve(&db).await.unwrap();
        std::env::remove_var("WEAVER_BRANCH");
        assert_eq!(resolved.id, b.id);
    }

    #[tokio::test]
    async fn resolve_from_a_real_git_checkout() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-b", "feature-x"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        let b = resolve_from_path(&db, repo.path()).await.unwrap();
        assert_eq!(b.branch, "feature-x");
        // Idempotent — second call returns the same row.
        let b2 = resolve_from_path(&db, repo.path()).await.unwrap();
        assert_eq!(b.id, b2.id);
    }
}
