//! Orchestrator-owned session rows. One *active* session per branch — terminal
//! sessions stay in history.

use anyhow::{anyhow, bail, Result};
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
    /// The watch id that owns this session when it is engine-managed
    /// infrastructure — a *warm session* a watcher keeps for its across-round
    /// memory. `None` for an ordinary fleet session. A managed session is hidden
    /// from the fleet listing ([`list_visible`]) and the survey scope, and its
    /// restart adoption is governed by `watch.adopt_warm` rather than
    /// `server.auto_adopt`.
    pub managed_by: Option<String>,
    /// The principal (username) that launched this session — attribution for the
    /// shared team board. `None` for engine-created sessions (warm watch
    /// sessions) and rows that predate the column. Stamped once at creation from
    /// the resolving [`crate::auth::Principal`]; a tracking/UX field, never a
    /// security boundary.
    pub created_by: Option<String>,
    /// Park state — the fleet list's resting shelf (a tier above archived: the
    /// terminal + worktree stay, the session stays resumable, it's just
    /// collapsed out of the live list). Tri-state: `None` = auto (parked-in-view
    /// once idle past the threshold, live otherwise), `Some("parked")` = pinned
    /// to the shelf by hand, `Some("active")` = kept live by hand even when idle.
    /// The idle threshold itself is a client view concern; the server only
    /// stores the explicit override.
    pub park: Option<String>,
    /// Manual fleet-list sort key. `None` = follow the automatic
    /// urgency-then-recency order; a number places the row exactly (assigned as
    /// the midpoint of its neighbours on drag), on one numeric axis with the
    /// derived auto-order so placed and untouched rows interleave.
    pub sort_order: Option<f64>,
    /// Execution backend: `"terminal"` (a PTY supervisor + interactive TUI) or
    /// `"acp"` (a headless adapter under a relay supervisor, driven by
    /// [`crate::acp`]). Defaults to `"terminal"`; rows predating the column read
    /// as terminal.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// The agent's own on-disk ACP session id, or `None` for a terminal session
    /// (or an ACP session before setup completes).
    pub acp_session_id: Option<String>,
    /// The relay spool cursor — the highest frame seq loom has durably journaled
    /// a block boundary for. [`crate::acp`] subscribes from here on (re)attach.
    #[serde(default)]
    pub acp_ack_seq: i64,
    /// Outstanding client->agent request state as JSON (`{"prompt_id":N,"turn":N}`),
    /// re-adopted on attach so a replayed turn-end response is recognized. `None`
    /// when no turn is in flight.
    pub acp_inflight: Option<String>,
    /// The session's current ACP mode id (gating posture), or `None` until the
    /// agent reports one.
    pub current_mode: Option<String>,
    /// The durable prompt queue: a paragraph-appended user message accumulated
    /// while a turn is in flight, dispatched as one prompt at the next turn
    /// boundary. Canonically the empty string when nothing is queued — the column
    /// is `NOT NULL DEFAULT ''`, so the queue-clearing writes store `''`, never
    /// NULL (an `Option` only because a legacy row may still hold NULL; treat
    /// `None` and `Some("")` identically). See [`take_pending_prompt`].
    pub pending_prompt: Option<String>,
    /// How this session came to exist: `"user"` (hand-launched), `"agent"`
    /// (delegated by another session), `"github"` / `"slack"` (chat triggers),
    /// `"watch"` (engine infrastructure). Stamped once at create, never
    /// re-derived.
    #[serde(default = "default_origin")]
    pub origin: String,
    /// Presentation tier: `"interactive"` fleet work or `"automation"`
    /// machinery, which the default fleet listing hides. Derived from `origin`
    /// at create (watch/actions/ops → automation), overridable per request.
    #[serde(default = "default_class")]
    pub class: String,
    /// Completed agent turns on this session, advanced at each turn boundary
    /// via [`increment_turn_count`].
    #[serde(default)]
    pub turn_count: i64,
    /// The weaver issue opened (or claimed) as this session's tracker at
    /// launch, or `None` when the launch tracked nothing.
    pub tracking_issue_id: Option<i64>,
    /// Named launch profile and its resolved non-secret policy snapshot.
    pub profile: String,
    pub launch_mode: String,
    pub profile_revision: i64,
    pub policy_env_clear: bool,
    pub policy_ambient_allowlist: String,
    pub policy_idle_archive_secs: Option<i64>,
    pub policy_turn_budget: i64,
    pub policy_prelude: String,
    pub policy_restricted: bool,
    pub policy_allowed_tools: String,
    /// Stable creator identity. `created_by` remains display attribution only.
    pub creator_kind: String,
    pub creator_subject: String,
    /// Immutable session ancestry for authorization; branch ancestry remains UI.
    pub parent_session_id: Option<String>,
    pub automation_run_id: Option<String>,
}

fn default_protocol() -> String {
    "terminal".to_string()
}

fn default_origin() -> String {
    "user".to_string()
}

fn default_class() -> String {
    "interactive".to_string()
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
    "created", "running", "orphaned", "done", "error", "archived",
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
    /// The owning watch id for an engine-managed (warm) session, or `None`
    /// for an ordinary fleet session. See [`Session::managed_by`].
    pub managed_by: Option<String>,
    /// The principal (username) that launched this session, or `None` for an
    /// engine-created (warm) session. See [`Session::created_by`].
    pub created_by: Option<String>,
    /// Execution backend, stamped once at create from the resolved agent/override
    /// and immutable thereafter: `"terminal"` or `"acp"`. See [`Session::protocol`].
    pub protocol: String,
    /// How this session came to exist. See [`Session::origin`].
    pub origin: String,
    /// `"interactive"` or `"automation"`. See [`Session::class`].
    pub class: String,
    /// The tracking issue opened/claimed for this session's task at launch, or
    /// `None` when there is nothing to track. See [`Session::tracking_issue_id`].
    pub tracking_issue_id: Option<i64>,
}

/// Resolved profile/security metadata stamped with a new session. Keeping this
/// separate preserves the compact `NewSession` fixture surface while real
/// runtime launches use [`insert_with_policy`].
pub struct SessionLaunchPolicy {
    pub profile: String,
    pub launch_mode: String,
    pub profile_revision: i64,
    pub env_clear: bool,
    pub ambient_allowlist: String,
    pub idle_archive_secs: Option<i64>,
    pub turn_budget: i64,
    pub prelude: String,
    pub restricted: bool,
    pub allowed_tools: String,
    pub creator_kind: String,
    pub creator_subject: String,
    pub parent_session_id: Option<String>,
    pub automation_run_id: Option<String>,
}

impl SessionLaunchPolicy {
    fn compatible(s: &NewSession) -> Self {
        let creator_kind = if s.origin == "agent" {
            "session"
        } else if matches!(s.origin.as_str(), "actions" | "ops") {
            "automation"
        } else if s.created_by.is_some() {
            "user"
        } else {
            "system"
        };
        Self {
            profile: crate::profile::DEFAULT_PROFILE.to_string(),
            launch_mode: crate::agent::DEFAULT_ACP_MODE.to_string(),
            profile_revision: 1,
            env_clear: false,
            ambient_allowlist: "[]".to_string(),
            idle_archive_secs: None,
            turn_budget: 0,
            prelude: "weaver".to_string(),
            restricted: false,
            allowed_tools: "[]".to_string(),
            creator_kind: creator_kind.to_string(),
            creator_subject: s.created_by.clone().unwrap_or_else(|| s.origin.clone()),
            parent_session_id: None,
            automation_run_id: None,
        }
    }
}

pub async fn insert(db: &Db, s: &NewSession) -> Result<Session> {
    insert_with_policy(db, s, &SessionLaunchPolicy::compatible(s)).await
}

pub async fn insert_with_policy(
    db: &Db,
    s: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<Session> {
    let now = now_iso();
    let protocol = if s.protocol.trim().is_empty() {
        "terminal"
    } else {
        s.protocol.trim()
    };
    sqlx::query(
        "INSERT INTO sessions
         (id, branch_id, work_dir, term_session, agent_kind, model, effort, status,
          github_repo, parent_branch_id, managed_by, created_by, protocol,
          origin, class, tracking_issue_id, last_activity_at, created_at,
          profile, launch_mode, profile_revision, policy_env_clear,
          policy_ambient_allowlist, policy_idle_archive_secs, policy_turn_budget,
          policy_prelude, policy_restricted, policy_allowed_tools,
          creator_kind, creator_subject, parent_session_id, automation_run_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(&s.created_by)
    .bind(protocol)
    .bind(&s.origin)
    .bind(&s.class)
    .bind(s.tracking_issue_id)
    .bind(&now)
    .bind(&now)
    .bind(&policy.profile)
    .bind(&policy.launch_mode)
    .bind(policy.profile_revision)
    .bind(policy.env_clear)
    .bind(&policy.ambient_allowlist)
    .bind(policy.idle_archive_secs)
    .bind(policy.turn_budget)
    .bind(&policy.prelude)
    .bind(policy.restricted)
    .bind(&policy.allowed_tools)
    .bind(&policy.creator_kind)
    .bind(&policy.creator_subject)
    .bind(&policy.parent_session_id)
    .bind(&policy.automation_run_id)
    .execute(db)
    .await?;
    tracing::info!(
        session = %s.id,
        branch = %s.branch_id,
        agent_kind = %s.agent_kind,
        status = %s.status,
        managed_by = s.managed_by.as_deref().unwrap_or("-"),
        parent_branch = s.parent_branch_id.as_deref().unwrap_or("-"),
        "session created"
    );
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

/// The active (non-terminal) session for a branch, if any. Archived counts as
/// terminal here — an archived session keeps its history row but no longer
/// occupies the branch slot — matching the `idx_sessions_active_branch`
/// predicate.
pub async fn active_for_branch(db: &Db, branch_id: &str) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE branch_id = ? AND status NOT IN ('done', 'error', 'archived')
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

/// The **fleet** sessions only — ordinary work, with engine-managed (warm) watch
/// sessions excluded. Rows from the removed concierge experiment stay hidden so
/// upgrading does not suddenly surface its infrastructure session as user work.
pub async fn list_visible(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE managed_by IS NULL AND agent_kind != 'concierge'
         ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Every engine-managed (warm) session — those owned by a watch. The
/// managed-session reconcile pass walks these to re-adopt a warm session whose
/// terminal is gone (when `watch.adopt_warm` is on) and to clean up one whose
/// owning watch has been deleted.
pub async fn list_managed(db: &Db) -> Result<Vec<Session>> {
    let rows = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions WHERE managed_by IS NOT NULL ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The owned (warm) session for a watch, if one exists and is not
/// terminal. Lets the engine reuse the same warm session across rounds rather
/// than spawning a duplicate.
pub async fn active_managed_by(db: &Db, watch_id: &str) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions
         WHERE managed_by = ? AND status NOT IN ('done', 'error', 'archived')
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(watch_id)
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

/// Count one completed agent turn: increment [`Session::turn_count`] and return
/// the new total. Called at each turn boundary (the monitor's terminal turn
/// detection, the ACP task's turn end).
pub async fn increment_turn_count(db: &Db, id: &str) -> Result<i64> {
    sqlx::query("UPDATE sessions SET turn_count = turn_count + 1 WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT turn_count FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_one(db)
        .await?;
    Ok(count)
}

/// Mark a live session orphaned after its terminal disappears.
///
/// The monitor discovers liveness from a fleet snapshot, which can be stale by
/// the time teardown finishes. Keep the terminal-state guard in the UPDATE so
/// an archive racing that snapshot cannot be overwritten back to `orphaned`.
/// Returns whether this call performed the transition.
pub async fn mark_orphaned(db: &Db, id: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions SET status = 'orphaned'
         WHERE id = ?
           AND status NOT IN ('orphaned', 'done', 'error', 'archived')",
    )
    .bind(id)
    .execute(db)
    .await?;
    let changed = result.rows_affected() == 1;
    if changed {
        tracing::info!(%id, "session marked orphaned atomically");
    }
    Ok(changed)
}

pub async fn touch(db: &Db, id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET last_activity_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    tracing::debug!(session = %id, "session activity touched");
    Ok(())
}

/// Set the manual park override — `Some("parked")` / `Some("active")` / `None`
/// (auto). See [`Session::park`]. The fleet list writes this when a row is
/// dragged into or out of the Parked shelf.
pub async fn set_park(db: &Db, id: &str, park: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE sessions SET park = ? WHERE id = ?")
        .bind(park)
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(session = %id, park = park.unwrap_or("auto"), "session park changed");
    Ok(())
}

/// Set the manual fleet-list sort key. See [`Session::sort_order`]. The list
/// writes the dragged row's new midpoint key here.
pub async fn set_sort_order(db: &Db, id: &str, order: f64) -> Result<()> {
    sqlx::query("UPDATE sessions SET sort_order = ? WHERE id = ?")
        .bind(order)
        .bind(id)
        .execute(db)
        .await?;
    tracing::debug!(session = %id, order, "session sort_order changed");
    Ok(())
}

/// Mark a session as ACP-backed and record the agent's on-disk session id (the
/// `session/new`/`session/load` id). Called by [`crate::acp::start`] once the
/// adapter has opened its session.
pub async fn set_acp(db: &Db, id: &str, acp_session_id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET protocol = 'acp', acp_session_id = ? WHERE id = ?")
        .bind(acp_session_id)
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(session = %id, acp_session_id, "session marked acp");
    Ok(())
}

/// Advance the persisted relay spool cursor to `seq` — the highest frame seq loom
/// has durably journaled a block boundary for. [`crate::acp`] subscribes from
/// this on (re)attach.
pub async fn set_ack_seq(db: &Db, id: &str, seq: i64) -> Result<()> {
    sqlx::query("UPDATE sessions SET acp_ack_seq = ? WHERE id = ?")
        .bind(seq)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Persist (or clear, with `None`) the outstanding client->agent request state —
/// the in-flight prompt id + turn — so a replayed turn-end response is recognized
/// after a loom restart. `inflight` is the JSON body or `None` to clear.
pub async fn set_inflight(db: &Db, id: &str, inflight: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE sessions SET acp_inflight = ? WHERE id = ?")
        .bind(inflight)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record the session's current ACP mode id (the gating posture).
pub async fn set_current_mode(db: &Db, id: &str, mode_id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET current_mode = ? WHERE id = ?")
        .bind(mode_id)
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(session = %id, mode = %mode_id, "session mode changed");
    Ok(())
}

/// Clear state owned by one ACP adapter process after setup fails. The stable
/// loom session, journal, runtime profile, and durable human prompt queue stay
/// intact so the failed session can be inspected or handed off.
pub async fn clear_acp_state(db: &Db, id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE sessions
         SET acp_session_id = NULL, acp_ack_seq = 0, acp_inflight = NULL,
             current_mode = NULL
         WHERE id = ?",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// The turn recorded in a session's durable ACP in-flight state, when valid.
pub fn acp_inflight_turn(session: &Session) -> Option<i64> {
    session
        .acp_inflight
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("turn").and_then(serde_json::Value::as_i64))
}

/// Replace an ACP session's runtime profile and clear every piece of
/// provider-private relay/session state. The journal and durable prompt queue
/// are deliberately untouched: both belong to loom's stable session and must
/// continue across a disconnected handoff.
pub async fn prepare_handoff(
    db: &Db,
    id: &str,
    agent_kind: &str,
    model: &str,
    effort: &str,
    status: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE sessions
         SET agent_kind = ?, model = ?, effort = ?, status = ?,
             acp_session_id = NULL, acp_ack_seq = 0, acp_inflight = NULL,
             current_mode = NULL
         WHERE id = ?",
    )
    .bind(agent_kind)
    .bind(model)
    .bind(effort)
    .bind(status)
    .bind(id)
    .execute(db)
    .await?;
    tracing::info!(session = %id, agent_kind, model, effort, status, "session runtime handed off");
    Ok(())
}

/// Append `text` to the durable prompt queue as a new paragraph (the queue holds
/// sends that arrived while a turn was in flight; it dispatches as one prompt at
/// the next turn boundary). Returns the full queued text after the append.
pub async fn append_pending_prompt(db: &Db, id: &str, text: &str) -> Result<String> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT pending_prompt FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .flatten();
    let combined = match existing.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(prev) => format!("{prev}\n\n{text}"),
        None => text.to_string(),
    };
    sqlx::query("UPDATE sessions SET pending_prompt = ? WHERE id = ?")
        .bind(&combined)
        .bind(id)
        .execute(db)
        .await?;
    Ok(combined)
}

/// Read the durable prompt queue (empty string when nothing is queued).
/// [`take_pending_prompt`] consumes it before the text is dispatched.
pub async fn read_pending_prompt(db: &Db, id: &str) -> Result<String> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT pending_prompt FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .flatten();
    Ok(existing.unwrap_or_default())
}

/// Atomically remove and return the durable prompt queue. A caller may dispatch
/// only a returned value: if the update fails, the transaction rolls back and
/// the prompt stays visibly queued instead of becoming eligible for replay at
/// every later turn boundary.
pub async fn take_pending_prompt(db: &Db, id: &str) -> Result<Option<String>> {
    // Take the writer lock before reading. A deferred transaction can read while
    // another writer holds WAL's reserved lock, then fail its read -> write
    // upgrade immediately with SQLITE_BUSY instead of honoring busy_timeout.
    // Stop-and-send reaches this just after persisting its cancellation boundary,
    // so that race used to leak "database is locked" to the composer.
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let pending: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT pending_prompt FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten()
            .filter(|text| !text.trim().is_empty());
    let Some(pending) = pending else {
        tx.commit().await?;
        return Ok(None);
    };

    let result = sqlx::query(
        // Clear to '' (the canonical empty), never NULL: the column is
        // `NOT NULL DEFAULT ''` on long-lived databases, so writing NULL here
        // fails the consume, the queue can never drain, and the conversation
        // wedges. See the module note on `pending_prompt`.
        "UPDATE sessions SET pending_prompt = ''
         WHERE id = ? AND pending_prompt = ?",
    )
    .bind(id)
    .bind(&pending)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        bail!("queued prompt changed while it was being consumed");
    }
    tx.commit().await?;
    Ok(Some(pending))
}

/// Remove a promoted prefix from the durable queue without dropping messages
/// appended behind it while the steering request was in flight.
pub async fn consume_pending_prompt(db: &Db, id: &str, promoted: &str) -> Result<()> {
    let current = read_pending_prompt(db, id).await?;
    // Canonically '' (never NULL) when the whole queue was promoted — the column
    // is `NOT NULL DEFAULT ''` on long-lived databases.
    let remaining: &str = if current == promoted {
        ""
    } else if let Some(rest) = current.strip_prefix(promoted) {
        rest.strip_prefix("\n\n")
            .ok_or_else(|| anyhow!("queued prompt changed while it was being steered"))?
    } else {
        bail!("queued prompt changed while it was being steered");
    };
    sqlx::query("UPDATE sessions SET pending_prompt = ? WHERE id = ?")
        .bind(remaining)
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
    tracing::info!(session = %id, "session row deleted");
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
            created_by: None,
            protocol: "terminal".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        }
    }

    /// `managed_by` round-trips and partitions the listings: `list` is the whole
    /// set, `list_visible` is the fleet (managed excluded), `list_managed` is the
    /// warm sessions (managed only).
    #[tokio::test]
    async fn managed_by_partitions_the_listings() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let ordinary_branch = branch_id(&db, "weaver/work").await;
        let warm_branch = branch_id(&db, "weaver/watch-x").await;

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
        assert_eq!(owned.id, "warm", "the watch's warm session resolves");
        assert!(
            active_managed_by(&db, "ov-other").await.unwrap().is_none(),
            "no warm session for a watch that owns none"
        );
    }

    /// Regression: archive kills the terminal before its final status write, so
    /// the monitor can still be holding a `running` fleet snapshot when it sees
    /// that terminal disappear. Its orphan transition must compare-and-set the
    /// current row, not overwrite a terminal status from that stale snapshot.
    #[tokio::test]
    async fn orphan_transition_cannot_resurrect_an_archived_session() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/archive-race").await;
        insert(&db, &new_session("archive-race", &branch, None))
            .await
            .unwrap();

        let stale_monitor_snapshot = get(&db, "archive-race").await.unwrap().unwrap();
        assert_eq!(stale_monitor_snapshot.status, "running");
        set_status(&db, "archive-race", "archived").await.unwrap();

        assert!(
            !mark_orphaned(&db, &stale_monitor_snapshot.id)
                .await
                .unwrap(),
            "terminal rows reject a stale monitor's orphan transition"
        );
        assert_eq!(
            get(&db, "archive-race").await.unwrap().unwrap().status,
            "archived"
        );
    }

    #[tokio::test]
    async fn orphan_transition_is_edge_triggered_for_a_live_session() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/orphan").await;
        insert(&db, &new_session("orphan", &branch, None))
            .await
            .unwrap();

        assert!(mark_orphaned(&db, "orphan").await.unwrap());
        assert_eq!(
            get(&db, "orphan").await.unwrap().unwrap().status,
            "orphaned"
        );
        assert!(
            !mark_orphaned(&db, "orphan").await.unwrap(),
            "an already-orphaned row does not emit another edge"
        );
    }

    #[tokio::test]
    async fn consuming_a_promoted_prompt_preserves_later_queue_entries() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/queue").await;
        insert(&db, &new_session("queue", &branch, None))
            .await
            .unwrap();
        append_pending_prompt(&db, "queue", "first").await.unwrap();
        append_pending_prompt(&db, "queue", "second").await.unwrap();

        consume_pending_prompt(&db, "queue", "first").await.unwrap();

        assert_eq!(read_pending_prompt(&db, "queue").await.unwrap(), "second");
    }

    #[tokio::test]
    async fn handoff_replaces_profile_and_clears_provider_state() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/handoff").await;
        let mut input = new_session("handoff", &branch, None);
        input.agent_kind = "claude".to_string();
        input.protocol = "acp".to_string();
        insert(&db, &input).await.unwrap();
        set_acp(&db, "handoff", "claude-private").await.unwrap();
        set_ack_seq(&db, "handoff", 99).await.unwrap();
        set_inflight(&db, "handoff", Some(r#"{"prompt_id":4,"turn":2}"#))
            .await
            .unwrap();
        set_current_mode(&db, "handoff", "acceptEdits")
            .await
            .unwrap();
        append_pending_prompt(&db, "handoff", "queued")
            .await
            .unwrap();

        prepare_handoff(&db, "handoff", "codex", "gpt-5.4", "high", "running")
            .await
            .unwrap();
        let session = get(&db, "handoff").await.unwrap().unwrap();
        assert_eq!(session.agent_kind, "codex");
        assert_eq!(session.model, "gpt-5.4");
        assert_eq!(session.effort, "high");
        assert_eq!(session.status, "running");
        assert!(session.acp_session_id.is_none());
        assert_eq!(session.acp_ack_seq, 0);
        assert!(session.acp_inflight.is_none());
        assert!(session.current_mode.is_none());
        assert_eq!(session.pending_prompt.as_deref(), Some("queued"));
    }

    /// Regression: the queue clears to `''`, never NULL. `sessions.pending_prompt`
    /// is `NOT NULL DEFAULT ''` (the shape long-lived databases carry), so a
    /// clearing write of NULL raises `NOT NULL constraint failed` — which used to
    /// make the queued prompt unconsumable and wedge the whole conversation. The
    /// in-memory schema now matches that shape, so this exercises the real
    /// constraint that shipped unguarded.
    #[tokio::test]
    async fn draining_the_queue_clears_to_empty_not_null() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // The column must actually carry the constraint, or this guards nothing.
        let notnull: i64 = sqlx::query_scalar(
            "SELECT \"notnull\" FROM pragma_table_info('sessions') WHERE name = 'pending_prompt'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(notnull, 1, "pending_prompt must be NOT NULL");

        let branch = branch_id(&db, "weaver/drain").await;
        insert(&db, &new_session("drain", &branch, None))
            .await
            .unwrap();
        append_pending_prompt(&db, "drain", "queued text")
            .await
            .unwrap();

        // take: the wedge path — this UPDATE used to write NULL and fail here.
        let taken = take_pending_prompt(&db, "drain").await.unwrap();
        assert_eq!(taken.as_deref(), Some("queued text"));
        let row = get(&db, "drain").await.unwrap().unwrap();
        assert_eq!(
            row.pending_prompt.as_deref(),
            Some(""),
            "cleared to '', not NULL"
        );

        // consume of the whole queue and a full handoff must clear the same way.
        append_pending_prompt(&db, "drain", "again").await.unwrap();
        consume_pending_prompt(&db, "drain", "again").await.unwrap();
        assert_eq!(read_pending_prompt(&db, "drain").await.unwrap(), "");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn take_pending_prompt_waits_for_a_concurrent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::connect(&dir.path().join("weaver.db"))
            .await
            .unwrap();
        let branch = branch_id(&db, "weaver/busy-queue").await;
        insert(&db, &new_session("busy-queue", &branch, None))
            .await
            .unwrap();
        append_pending_prompt(&db, "busy-queue", "send after stop")
            .await
            .unwrap();

        let writer = weaver_core::db::begin_immediate(&db).await.unwrap();
        let contender_db = db.clone();
        let take =
            tokio::spawn(async move { take_pending_prompt(&contender_db, "busy-queue").await });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !take.is_finished(),
            "queue consumption should wait for the writer instead of returning SQLITE_BUSY"
        );
        writer.commit().await.unwrap();

        let taken = tokio::time::timeout(std::time::Duration::from_secs(1), take)
            .await
            .expect("queue consumption resumes once the writer commits")
            .unwrap()
            .unwrap();
        assert_eq!(taken.as_deref(), Some("send after stop"));
    }
}
