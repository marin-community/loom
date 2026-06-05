//! Orchestrator-owned session rows. One *active* session per branch — terminal
//! sessions stay in history.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};
use weaver_core::branch::{self as branch_mod, Branch};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub id: String,
    pub branch_id: String,
    pub work_dir: String,
    pub tmux_session: String,
    pub agent_kind: String,
    /// Model tier ('', 'haiku', 'sonnet', 'opus') — spliced in as `--model`.
    pub model: String,
    /// Reasoning effort ('', 'low', 'medium', 'high', 'xhigh', 'max') — `--effort`.
    pub effort: String,
    pub status: String,
    pub github_repo: Option<String>,
    pub last_activity_at: Option<String>,
    pub created_at: String,
}

/// Session **lifecycle** states — the mechanical, orchestrator-owned axis: is
/// the agent process being set up, alive, lost, or finished. How the agent is
/// *doing* (whether it needs the user) is the separate, agent-declared
/// `attention` axis on the branch — see [`weaver_core::branch::ATTENTION_LEVELS`].
///
/// `running` replaces the old inferred `working`/`waiting`/`idle` trio: those
/// guessed at the agent's state from hooks and screen stillness and were
/// frequently wrong (e.g. an agent waiting on a background workflow looked
/// "idle"). Liveness is all the orchestrator can know for sure; the agent
/// reports the rest via `weaver set-status`.
pub const STATUSES: &[&str] = &[
    "created",
    "launching",
    "running",
    "orphaned",
    "done",
    "error",
    "archived",
];

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "error" | "archived")
}

pub struct NewSession {
    pub id: String,
    pub branch_id: String,
    pub work_dir: String,
    pub tmux_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub status: String,
    pub github_repo: Option<String>,
}

pub async fn insert(db: &Db, s: &NewSession) -> Result<Session> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO sessions
         (id, branch_id, work_dir, tmux_session, agent_kind, model, effort, status,
          github_repo, last_activity_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&s.id)
    .bind(&s.branch_id)
    .bind(&s.work_dir)
    .bind(&s.tmux_session)
    .bind(&s.agent_kind)
    .bind(&s.model)
    .bind(&s.effort)
    .bind(&s.status)
    .bind(&s.github_repo)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    get(db, &s.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session vanished after insert"))
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

/// The active (non-terminal) session for a branch, if any.
pub async fn active_for_branch(db: &Db, branch_id: &str) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE branch_id = ? AND status NOT IN ('done', 'error')
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(branch_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

pub async fn list(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>("SELECT * FROM sessions ORDER BY created_at DESC")
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// `(Session, Branch)` for a session id. None if the session is missing.
pub async fn with_branch(db: &Db, id: &str) -> Result<Option<(Session, Branch)>> {
    let Some(session) = get(db, id).await? else {
        return Ok(None);
    };
    let Some(branch) = branch_mod::get(db, &session.branch_id).await? else {
        return Ok(None);
    };
    Ok(Some((session, branch)))
}

pub async fn set_status(db: &Db, id: &str, status: &str) -> Result<()> {
    let old: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);
    sqlx::query("UPDATE sessions SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(
        %id,
        old = old.as_deref().unwrap_or("?"),
        new = %status,
        "session status changed"
    );
    Ok(())
}

pub async fn touch(db: &Db, id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET last_activity_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
