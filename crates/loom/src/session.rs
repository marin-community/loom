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
    pub term_session: String,
    pub agent_kind: String,
    /// Model tier ('', 'haiku', 'sonnet', 'opus', 'fable') — spliced in as
    /// `--model`.
    pub model: String,
    /// Reasoning effort ('', 'low', 'medium', 'high', 'xhigh', 'max') — `--effort`.
    pub effort: String,
    pub status: String,
    pub github_repo: Option<String>,
    pub last_activity_at: Option<String>,
    pub created_at: String,
    /// Branch id of the session that launched this one — its parent in the
    /// dashboard's session tree. `None` for a top-level session. Set once at
    /// creation from the resolved launcher, never re-derived.
    pub parent_branch_id: Option<String>,
    /// The overlooker id that owns this session when it is engine-managed
    /// infrastructure — a *warm session* a watcher keeps for its across-round
    /// memory. `None` for an ordinary fleet session. A managed session is hidden
    /// from the fleet listing ([`list_visible`]) and the survey scope, and its
    /// restart adoption is governed by `overlooker.adopt_warm` rather than
    /// `server.auto_adopt`.
    pub managed_by: Option<String>,
}

/// Session **lifecycle** states — the mechanical, orchestrator-owned axis: is
/// the agent process being set up, alive, lost, or finished. How the agent is
/// *doing* (whether it needs the user) is the separate, agent-declared
/// `attention` axis — the branch's `attention` tag, see
/// [`weaver_core::tags::ATTENTION_KEY`].
///
/// `running` replaces the old inferred `working`/`waiting`/`idle` trio: those
/// guessed at the agent's state from hooks and screen stillness and were
/// frequently wrong (e.g. an agent waiting on a background workflow looked
/// "idle"). Liveness is all the orchestrator can know for sure; the agent
/// reports the rest via `weaver status`.
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
    pub term_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub status: String,
    pub github_repo: Option<String>,
    /// Branch id of the launching session (the parent in the session tree), or
    /// `None` for a top-level launch. See [`Session::parent_branch_id`].
    pub parent_branch_id: Option<String>,
    /// The owning overlooker id for an engine-managed (warm) session, or `None`
    /// for an ordinary fleet session. See [`Session::managed_by`].
    pub managed_by: Option<String>,
}

pub async fn insert(db: &Db, s: &NewSession) -> Result<Session> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO sessions
         (id, branch_id, work_dir, term_session, agent_kind, model, effort, status,
          github_repo, parent_branch_id, managed_by, last_activity_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&s.id)
    .bind(&s.branch_id)
    .bind(&s.work_dir)
    .bind(&s.term_session)
    .bind(&s.agent_kind)
    .bind(&s.model)
    .bind(&s.effort)
    .bind(&s.status)
    .bind(&s.github_repo)
    .bind(&s.parent_branch_id)
    .bind(&s.managed_by)
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

/// Every session, ordered newest-first — managed (warm) sessions included. The
/// internal view: the monitor's liveness walk, the adopt reconcile, and any
/// engine bookkeeping use this so a managed session is never dropped from
/// orphan detection. The fleet/dashboard listing and the survey scope use
/// [`list_visible`] instead.
pub async fn list(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>("SELECT * FROM sessions ORDER BY created_at DESC")
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// The **fleet** sessions only — ordinary work, with infrastructure sessions
/// excluded: engine-managed (warm) overlooker sessions, and the fleet
/// **concierge** (the Chat agent, which watches the fleet rather than being part
/// of it). Neither is work to show or survey, so the dashboard `/sessions`
/// listing and an overlooker round's scope survey both read this list.
pub async fn list_visible(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE managed_by IS NULL AND agent_kind != ?
         ORDER BY created_at DESC",
    )
    .bind(crate::agent::CONCIERGE_KIND)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The live fleet **concierge** session, if one exists and is not terminal. The
/// Chat surface resolves its singleton through this — find the running concierge,
/// else create one. Hidden from [`list_visible`], so it never shows in the fleet.
pub async fn active_concierge(db: &Db) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE agent_kind = ? AND status NOT IN ('done', 'error', 'archived')
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(crate::agent::CONCIERGE_KIND)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Every engine-managed (warm) session — those owned by an overlooker. The
/// managed-session reconcile pass walks these to re-adopt a warm session whose
/// terminal is gone (when `overlooker.adopt_warm` is on) and to clean up one whose
/// owning overlooker has been deleted.
pub async fn list_managed(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions WHERE managed_by IS NOT NULL ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The owned (warm) session for an overlooker, if one exists and is not
/// terminal. Lets the engine reuse the same warm session across rounds rather
/// than spawning a duplicate.
pub async fn active_managed_by(db: &Db, overlooker_id: &str) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE managed_by = ? AND status NOT IN ('done', 'error', 'archived')
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(overlooker_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn branch_id(db: &Db, name: &str) -> String {
        branch_mod::upsert(db, "/repo", name, "main")
            .await
            .unwrap()
            .id
    }

    fn new_session(id: &str, branch_id: &str, managed_by: Option<&str>) -> NewSession {
        NewSession {
            id: id.to_string(),
            branch_id: branch_id.to_string(),
            work_dir: "/w".to_string(),
            term_session: format!("weaver-{id}"),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: managed_by.map(str::to_string),
        }
    }

    /// `managed_by` round-trips and partitions the listings: `list` is the whole
    /// set, `list_visible` is the fleet (managed excluded), `list_managed` is the
    /// warm sessions (managed only).
    #[tokio::test]
    async fn managed_by_partitions_the_listings() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let ordinary_branch = branch_id(&db, "weaver/work").await;
        let warm_branch = branch_id(&db, "weaver/overlooker-x").await;

        insert(&db, &new_session("ordinary", &ordinary_branch, None))
            .await
            .unwrap();
        let warm = insert(&db, &new_session("warm", &warm_branch, Some("ov-1")))
            .await
            .unwrap();
        assert_eq!(warm.managed_by.as_deref(), Some("ov-1"), "marker persists");

        let all: Vec<String> = list(&db).await.unwrap().into_iter().map(|s| s.id).collect();
        assert!(all.contains(&"ordinary".to_string()) && all.contains(&"warm".to_string()));

        let visible: Vec<String> = list_visible(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(visible, vec!["ordinary".to_string()], "fleet hides managed");

        let managed: Vec<String> = list_managed(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(managed, vec!["warm".to_string()], "only managed listed");

        let owned = active_managed_by(&db, "ov-1").await.unwrap().unwrap();
        assert_eq!(owned.id, "warm", "the overlooker's warm session resolves");
        assert!(
            active_managed_by(&db, "ov-other").await.unwrap().is_none(),
            "no warm session for an overlooker that owns none"
        );
    }
}
