//! Orchestrator-owned session rows. One *active* session per branch — terminal
//! sessions stay in history.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row, SqliteConnection};

use crate::channel_data::{
    MessageKind, SubjectKind, SubscriptionMode, Urgency, OPEN_STATE, SESSION_KIND,
};
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
    /// A durable external lifecycle operation currently in flight. The stable
    /// `status` remains the last completed state until the operation commits.
    pub lifecycle_transition: Option<String>,
    /// Human-readable current stage of [`Self::lifecycle_transition`].
    pub lifecycle_step: Option<String>,
    pub lifecycle_transition_started_at: Option<String>,
    /// Loom process that claimed the transition, used to avoid stealing work
    /// from an older generation that is still draining during a rolling restart.
    pub lifecycle_transition_owner_pid: Option<i64>,
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
    /// Historical input retained so the layout migration can map explicit
    /// `parked` rows into a normal `Later` group. API reads are derived from
    /// canonical placement and no runtime path writes this column.
    pub park: Option<String>,
    /// Historical ordering input retained for migration only. API reads use
    /// canonical integer placement rank and no runtime path writes this column.
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
    /// Presentation tier: `"interactive"` or `"automation"`. Both are normal
    /// fleet sessions; the machine fact still drives authorization and issue
    /// policies. Derived from `origin` at create, overridable per request.
    #[serde(default = "default_class")]
    pub class: String,
    /// Completed agent turns on this session, advanced at each turn boundary
    /// via [`increment_turn_count`].
    #[serde(default)]
    pub turn_count: i64,
    /// An explicitly claimed/imported compatibility work item, or `None` for
    /// an ordinary launch whose coordination lives in its default channel.
    pub tracking_issue_id: Option<i64>,
    /// Named launch profile and its resolved non-secret policy snapshot.
    pub profile: String,
    pub launch_mode: String,
    pub profile_revision: i64,
    /// Stable profile lifetime accepted at launch. Zero means an upgraded row
    /// whose relationship to the current same-name profile cannot be proven.
    pub profile_lifetime: i64,
    /// Immutable environment precedence chosen at launch.
    pub policy_strict: bool,
    pub policy_env_clear: bool,
    pub policy_ambient_allowlist: String,
    pub policy_idle_archive_secs: Option<i64>,
    pub policy_turn_budget: i64,
    pub policy_prelude: String,
    pub policy_restricted: bool,
    pub policy_github_repositories: String,
    pub policy_allowed_tools: String,
    pub policy_mcp_access: String,
    /// Canonical source-redacted launch resolution JSON. Empty only for rows
    /// created before the launch-composition contract.
    pub launch_snapshot: String,
    /// Stable creator identity. `created_by` remains display attribution only.
    pub creator_kind: String,
    pub creator_subject: String,
    /// Immutable session ancestry for authorization; branch ancestry remains UI.
    pub parent_session_id: Option<String>,
    pub automation_run_id: Option<String>,
    /// Optimistic generation for lifecycle/status/goal ordering.
    pub mutation_revision: i64,
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

const SESSION_COLUMNS: &str = "\
    id, branch_id, work_dir, term_session, agent_kind, model, effort, status, \
    lifecycle_transition, lifecycle_step, lifecycle_transition_started_at, \
    lifecycle_transition_owner_pid, github_repo, \
    last_activity_at, created_at, parent_branch_id, managed_by, created_by, park, sort_order, \
    protocol, acp_session_id, acp_ack_seq, acp_inflight, current_mode, pending_prompt, origin, \
    class, turn_count, tracking_issue_id, profile, launch_mode, profile_revision, \
    profile_lifetime, policy_strict, policy_env_clear, policy_ambient_allowlist, \
    policy_idle_archive_secs, policy_turn_budget, policy_prelude, policy_restricted, \
    policy_github_repositories, policy_allowed_tools, policy_mcp_access, launch_snapshot, creator_kind, creator_subject, \
    parent_session_id, automation_run_id, mutation_revision";

fn select_sessions(suffix: &str) -> String {
    // The HTTP listener closes before long-lived streams finish draining during
    // a rolling restart. Keep each process generation's row shape independent
    // while a replacement adds columns: SQLite can recompile `SELECT *` after
    // sqlx captured its metadata and otherwise panic on the widened row.
    format!("SELECT {SESSION_COLUMNS} FROM sessions {suffix}")
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
/// reports the rest via `loom status`.
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
    /// An explicitly claimed/imported compatibility work item, or `None` for
    /// an ordinary channel-coordinated launch. See [`Session::tracking_issue_id`].
    pub tracking_issue_id: Option<i64>,
}

/// Resolved profile/security metadata stamped with a new session. Keeping this
/// separate preserves the compact `NewSession` fixture surface while real
/// runtime launches use [`insert_with_policy`].
pub struct SessionLaunchPolicy {
    pub profile: String,
    pub launch_mode: String,
    pub profile_revision: i64,
    pub profile_lifetime: i64,
    pub strict: bool,
    pub env_clear: bool,
    pub ambient_allowlist: String,
    pub idle_archive_secs: Option<i64>,
    pub turn_budget: i64,
    pub prelude: String,
    pub restricted: bool,
    pub github_repositories: String,
    pub allowed_tools: String,
    pub mcp_access: String,
    pub launch_snapshot: String,
    pub creator_kind: String,
    pub creator_subject: String,
    pub parent_session_id: Option<String>,
    pub automation_run_id: Option<String>,
}

/// Resolved profile/security metadata replaced during an ACP handoff. Creator
/// identity and session ancestry remain immutable.
pub struct SessionHandoffPolicy {
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub profile: String,
    pub launch_mode: String,
    pub profile_revision: i64,
    pub profile_lifetime: i64,
    pub strict: bool,
    pub env_clear: bool,
    pub ambient_allowlist: String,
    pub idle_archive_secs: Option<i64>,
    pub turn_budget: i64,
    pub prelude: String,
    pub restricted: bool,
    pub github_repositories: String,
    pub allowed_tools: String,
    pub mcp_access: String,
    pub launch_snapshot: String,
}

impl SessionLaunchPolicy {
    fn compatible(s: &NewSession) -> Self {
        let creator_kind = if s.origin == "agent" {
            SubjectKind::Session
        } else if matches!(s.origin.as_str(), "actions" | "ops") {
            SubjectKind::Automation
        } else if s.created_by.is_some() {
            SubjectKind::User
        } else {
            SubjectKind::System
        };
        Self {
            profile: crate::agent_kind::DEFAULT_PROFILE.to_string(),
            launch_mode: crate::agent_kind::DEFAULT_ACP_MODE.to_string(),
            profile_revision: 1,
            profile_lifetime: 1,
            strict: false,
            env_clear: false,
            ambient_allowlist: "[]".to_string(),
            idle_archive_secs: None,
            turn_budget: 0,
            prelude: "weaver".to_string(),
            restricted: false,
            github_repositories: "[]".to_string(),
            allowed_tools: "[]".to_string(),
            mcp_access: r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[]}"#
                .to_string(),
            launch_snapshot: String::new(),
            creator_kind: creator_kind.as_str().to_string(),
            creator_subject: s.created_by.clone().unwrap_or_else(|| s.origin.clone()),
            parent_session_id: None,
            automation_run_id: None,
        }
    }
}

async fn current_layout_revision_tx(tx: &mut SqliteConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
        .fetch_one(tx)
        .await
}

async fn bump_layout_revision_tx(tx: &mut SqliteConnection) -> sqlx::Result<()> {
    sqlx::query("UPDATE session_layout_state SET revision = revision + 1 WHERE id = 1")
        .execute(tx)
        .await?;
    Ok(())
}

/// Add the canonical placement while the session insertion transaction is
/// still open. Session creation owns this invariant; the layout module owns
/// later user-driven layout mutations.
async fn insert_default_placement_tx(
    tx: &mut SqliteConnection,
    session: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<Option<i64>> {
    if session.managed_by.is_some() {
        return Ok(None);
    }
    let inherited = if session.origin == "agent" {
        if let Some(parent_id) = policy.parent_session_id.as_deref() {
            sqlx::query_scalar::<_, String>(
                "SELECT group_id FROM session_placements WHERE session_id = ?",
            )
            .bind(parent_id)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            None
        }
    } else {
        None
    };
    let mut group_id = inherited;
    if group_id.is_none() {
        let watch_id = if let Some(run_id) = policy.automation_run_id.as_deref() {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT json_extract(request_json, '$.watch_id')
                 FROM automation_runs WHERE id = ?",
            )
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten()
        } else {
            None
        };
        let selectors = [
            watch_id.as_deref().map(|id| ("watch", id)),
            Some(("profile", policy.profile.as_str())),
            Some(("origin", session.origin.as_str())),
            Some(("origin", "*")),
        ];
        for (kind, value) in selectors.into_iter().flatten() {
            group_id = sqlx::query_scalar::<_, String>(
                "SELECT group_id FROM session_placement_defaults
                 WHERE selector_kind = ? AND selector_value = ?",
            )
            .bind(kind)
            .bind(value)
            .fetch_optional(&mut *tx)
            .await?;
            if group_id.is_some() {
                break;
            }
        }
    }
    let group_id = group_id.unwrap_or_else(|| "group-user-inbox".to_string());
    let rank: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(rank) + 1, 0) FROM session_placements WHERE group_id = ?",
    )
    .bind(&group_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_placements (session_id, group_id, rank, updated_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(group_id)
    .bind(rank)
    .bind(now_iso())
    .execute(&mut *tx)
    .await?;
    bump_layout_revision_tx(tx).await?;
    Ok(Some(current_layout_revision_tx(tx).await?))
}

async fn upsert_initial_subscription_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject_kind: SubjectKind,
    subject_id: &str,
    mode: SubscriptionMode,
    read_seq: i64,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_subscriptions
         (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, subject_kind, subject_id) DO UPDATE SET
           mode = excluded.mode,
           read_seq = MAX(channel_subscriptions.read_seq, excluded.read_seq),
           updated_at = excluded.updated_at",
    )
    .bind(channel_id)
    .bind(subject_kind.as_str())
    .bind(subject_id)
    .bind(mode.as_str())
    .bind(read_seq)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Insert the default communication channel as part of session creation.
async fn insert_session_channel_tx(
    tx: &mut SqliteConnection,
    session: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<()> {
    let row = sqlx::query("SELECT repo_root, title, goal FROM branches WHERE id = ?")
        .bind(&session.branch_id)
        .fetch_one(&mut *tx)
        .await?;
    let repo_root: String = row.get("repo_root");
    let title: String = row.get("title");
    let goal: String = row.get("goal");
    let now = now_iso();
    let creator_kind = SubjectKind::parse(&policy.creator_kind)
        .ok_or_else(|| anyhow!("unknown channel subject kind '{}'", policy.creator_kind))?;
    sqlx::query(
        "INSERT INTO channels
         (id, kind, repo_root, branch_id, session_id, name, topic, state,
          created_by_kind, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(SESSION_KIND)
    .bind(repo_root)
    .bind(&session.branch_id)
    .bind(&session.id)
    .bind(if title.trim().is_empty() {
        &session.id
    } else {
        title.trim()
    })
    .bind(&goal)
    .bind(OPEN_STATE)
    .bind(creator_kind.as_str())
    .bind(&policy.creator_subject)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    let goal_seq = if goal.trim().is_empty() {
        0
    } else {
        sqlx::query(
            "INSERT INTO channel_messages
             (id, channel_id, seq, kind, urgency, author_kind, author_id,
              body, payload, created_at)
             VALUES (?, ?, 1, ?, ?, ?, ?, ?, '{}', ?)",
        )
        .bind(weaver_core::branch::new_id())
        .bind(&session.id)
        .bind(MessageKind::Goal.as_str())
        .bind(Urgency::Normal.as_str())
        .bind(creator_kind.as_str())
        .bind(&policy.creator_subject)
        .bind(goal)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        1
    };

    upsert_initial_subscription_tx(
        tx,
        &session.id,
        SubjectKind::Session,
        &session.id,
        SubscriptionMode::Deliver,
        goal_seq,
        &now,
    )
    .await?;
    upsert_initial_subscription_tx(
        tx,
        &session.id,
        creator_kind,
        &policy.creator_subject,
        SubscriptionMode::Observe,
        goal_seq,
        &now,
    )
    .await?;
    if let Some(username) = session.created_by.as_deref() {
        upsert_initial_subscription_tx(
            tx,
            &session.id,
            SubjectKind::User,
            username,
            SubscriptionMode::Observe,
            goal_seq,
            &now,
        )
        .await?;
    }
    if let Some(parent) = policy.parent_session_id.as_deref() {
        upsert_initial_subscription_tx(
            tx,
            &session.id,
            SubjectKind::Session,
            parent,
            SubscriptionMode::Observe,
            goal_seq,
            &now,
        )
        .await?;
    }
    Ok(())
}

pub async fn insert(db: &Db, s: &NewSession) -> Result<Session> {
    insert_with_policy(db, s, &SessionLaunchPolicy::compatible(s)).await
}

pub async fn insert_with_policy(
    db: &Db,
    s: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<Session> {
    Ok(insert_with_layout_revision(db, s, policy).await?.0)
}

pub(crate) async fn insert_with_layout_revision(
    db: &Db,
    s: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<(Session, Option<i64>)> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
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
          profile, launch_mode, profile_revision, profile_lifetime, policy_strict,
          policy_env_clear,
          policy_ambient_allowlist, policy_idle_archive_secs, policy_turn_budget,
          policy_prelude, policy_restricted, policy_github_repositories, policy_allowed_tools, policy_mcp_access,
          launch_snapshot, creator_kind, creator_subject, parent_session_id, automation_run_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(policy.profile_lifetime)
    .bind(policy.strict)
    .bind(policy.env_clear)
    .bind(&policy.ambient_allowlist)
    .bind(policy.idle_archive_secs)
    .bind(policy.turn_budget)
    .bind(&policy.prelude)
    .bind(policy.restricted)
    .bind(&policy.github_repositories)
    .bind(&policy.allowed_tools)
    .bind(&policy.mcp_access)
    .bind(&policy.launch_snapshot)
    .bind(&policy.creator_kind)
    .bind(&policy.creator_subject)
    .bind(&policy.parent_session_id)
    .bind(&policy.automation_run_id)
    .execute(&mut *tx)
    .await?;
    let layout_revision = insert_default_placement_tx(&mut tx, s, policy).await?;
    insert_session_channel_tx(&mut tx, s, policy).await?;
    tx.commit().await?;
    tracing::info!(
        session = %s.id,
        branch = %s.branch_id,
        agent_kind = %s.agent_kind,
        status = %s.status,
        managed_by = s.managed_by.as_deref().unwrap_or("-"),
        parent_branch = s.parent_branch_id.as_deref().unwrap_or("-"),
        "session created"
    );
    let session = get(db, &s.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session vanished after insert"))?;
    Ok((session, layout_revision))
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Session>> {
    let query = select_sessions("WHERE id = ?");
    let row = sqlx::query_as::<_, Session>(&query)
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
    let query = select_sessions(
        "WHERE branch_id = ? AND status NOT IN ('done', 'error', 'archived')
         ORDER BY created_at DESC
         LIMIT 1",
    );
    let row = sqlx::query_as::<_, Session>(&query)
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
    let query = select_sessions("ORDER BY created_at DESC");
    let rows = sqlx::query_as::<_, Session>(&query).fetch_all(db).await?;
    Ok(rows)
}

/// The **fleet** sessions only — ordinary work, with engine-managed (warm) watch
/// sessions excluded. Rows from the removed concierge experiment stay hidden so
/// upgrading does not suddenly surface its infrastructure session as user work.
pub async fn list_visible(db: &Db) -> Result<Vec<Session>> {
    let query = select_sessions(
        "WHERE managed_by IS NULL AND agent_kind != 'concierge'
         ORDER BY created_at DESC",
    );
    let rows = sqlx::query_as::<_, Session>(&query).fetch_all(db).await?;
    Ok(rows)
}

/// Every engine-managed (warm) session — those owned by a watch. The
/// managed-session reconcile pass walks these to re-adopt a warm session whose
/// terminal is gone (when `watch.adopt_warm` is on) and to clean up one whose
/// owning watch has been deleted.
pub async fn list_managed(db: &Db) -> Result<Vec<Session>> {
    let query = select_sessions("WHERE managed_by IS NOT NULL ORDER BY created_at DESC");
    let rows = sqlx::query_as::<_, Session>(&query).fetch_all(db).await?;
    Ok(rows)
}

/// The owned (warm) session for a watch, if one exists and is not
/// terminal. Lets the engine reuse the same warm session across rounds rather
/// than spawning a duplicate.
pub async fn active_managed_by(db: &Db, watch_id: &str) -> Result<Option<Session>> {
    let query = select_sessions(
        "WHERE managed_by = ? AND status NOT IN ('done', 'error', 'archived')
         ORDER BY created_at DESC
         LIMIT 1",
    );
    let row = sqlx::query_as::<_, Session>(&query)
        .bind(watch_id)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

/// Every session on a branch that can currently enter provider handoff.
///
/// Error rows are intentionally included even though they do not consume
/// capacity: a branch-goal edit must advance their mutation generation too, or
/// a handoff can commit a prompt built from the superseded goal.
pub async fn handoff_capable_for_branch(db: &Db, branch_id: &str) -> Result<Vec<Session>> {
    let query = select_sessions(
        "WHERE branch_id = ?
           AND protocol = 'acp'
           AND status IN ('running', 'orphaned', 'error', 'handoff')
         ORDER BY id",
    );
    Ok(sqlx::query_as::<_, Session>(&query)
        .bind(branch_id)
        .fetch_all(db)
        .await?)
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

/// Resolve a session key — a session id, a branch id, a branch name, or
/// `repo:branch` — to the live session behind it and its branch.
///
/// `None` when nothing active answers to `key`: either the key names nothing at
/// all, or it names a branch whose session has already reached a terminal state.
/// Callers that need to distinguish "no such thing" from "nothing running" can
/// fall back to [`weaver_core::branch::resolve_key`] themselves.
pub async fn resolve_key(db: &Db, key: &str) -> Result<Option<(Session, Branch)>> {
    if let Some(pair) = with_branch(db, key).await? {
        return Ok(Some(pair));
    }
    let Some(branch) = branch_mod::resolve_key(db, key).await? else {
        return Ok(None);
    };
    Ok(active_for_branch(db, &branch.id)
        .await?
        .map(|session| (session, branch)))
}

pub async fn set_status(db: &Db, id: &str, status: &str) -> Result<()> {
    let old: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);
    sqlx::query(
        "UPDATE sessions
         SET status = ?, mutation_revision = mutation_revision + 1
         WHERE id = ?",
    )
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

/// Begin an externally visible lifecycle transition if no other operation owns
/// the row. This database guard complements the process-local lifecycle lock
/// during rolling restarts, where two loom generations can briefly overlap.
pub async fn begin_transition(db: &Db, id: &str, transition: &str, step: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET lifecycle_transition = ?, lifecycle_step = ?,
             lifecycle_transition_started_at = ?, lifecycle_transition_owner_pid = ?,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition IS NULL",
    )
    .bind(transition)
    .bind(step)
    .bind(now_iso())
    .bind(i64::from(std::process::id()))
    .bind(id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Advance the explanatory stage without allowing a stale operation to update
/// a newer transition.
pub async fn update_transition_step(
    db: &Db,
    id: &str,
    transition: &str,
    step: &str,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET lifecycle_step = ?, mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition = ? AND lifecycle_transition_owner_pid = ?",
    )
    .bind(step)
    .bind(id)
    .bind(transition)
    .bind(i64::from(std::process::id()))
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn clear_transition(db: &Db, id: &str, transition: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET lifecycle_transition = NULL, lifecycle_step = NULL,
             lifecycle_transition_started_at = NULL, lifecycle_transition_owner_pid = NULL,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition = ? AND lifecycle_transition_owner_pid = ?",
    )
    .bind(id)
    .bind(transition)
    .bind(i64::from(std::process::id()))
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Commit the stable lifecycle state and clear its in-flight presentation in
/// one SQLite boundary, so readers never observe a completed state carrying a
/// stale progress label.
pub async fn complete_transition(
    db: &Db,
    id: &str,
    transition: &str,
    status: &str,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET status = ?, lifecycle_transition = NULL, lifecycle_step = NULL,
             lifecycle_transition_started_at = NULL, lifecycle_transition_owner_pid = NULL,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition = ? AND lifecycle_transition_owner_pid = ?",
    )
    .bind(status)
    .bind(id)
    .bind(transition)
    .bind(i64::from(std::process::id()))
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Release a transition whose recorded owner process is confirmed gone during
/// startup reconciliation. Normal operation code must use [`clear_transition`]
/// so an old generation cannot clear a newer generation's work.
pub async fn clear_interrupted_transition(db: &Db, id: &str, transition: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET lifecycle_transition = NULL, lifecycle_step = NULL,
             lifecycle_transition_started_at = NULL, lifecycle_transition_owner_pid = NULL,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition = ?",
    )
    .bind(id)
    .bind(transition)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Commit a stale owner's interrupted transition after startup has established
/// that process is gone. See [`clear_interrupted_transition`].
pub async fn complete_interrupted_transition(
    db: &Db,
    id: &str,
    transition: &str,
    status: &str,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET status = ?, lifecycle_transition = NULL, lifecycle_step = NULL,
             lifecycle_transition_started_at = NULL, lifecycle_transition_owner_pid = NULL,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND lifecycle_transition = ?",
    )
    .bind(status)
    .bind(id)
    .bind(transition)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Join a branch-goal mutation to the same optimistic ordering boundary as
/// status/lifecycle changes.
pub async fn bump_mutation_revision(db: &Db, id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE sessions
         SET mutation_revision = mutation_revision + 1
         WHERE id = ?",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Atomically reserve an archived session's branch slot for recovery.
///
/// `archived` rows do not participate in the one-active-session-per-branch
/// index. Moving to `created` before rebuilding a worktree or launching a
/// supervisor makes SQLite arbitrate a concurrent re-let of that slot before
/// any external state is touched. Returns false when the row is no longer
/// archived (including a concurrent recovery that already claimed it).
pub async fn claim_recovery(db: &Db, id: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET status = 'created', lifecycle_transition = 'adopting',
             lifecycle_step = 'Preparing recovery', lifecycle_transition_started_at = ?,
             lifecycle_transition_owner_pid = ?, mutation_revision = mutation_revision + 1
         WHERE id = ? AND status = 'archived' AND lifecycle_transition IS NULL",
    )
    .bind(now_iso())
    .bind(i64::from(std::process::id()))
    .bind(id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
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
/// A handoff owns an intentional supervisor-free interval while replacing the
/// provider, so the monitor must not invalidate its mutation generation either.
/// Returns whether this call performed the transition.
pub async fn mark_orphaned(db: &Db, id: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET status = 'orphaned', mutation_revision = mutation_revision + 1
         WHERE id = ?
           AND status NOT IN ('orphaned', 'done', 'error', 'archived', 'handoff')",
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

/// Store the latest complete provider-owned ACP composer metadata. It remains
/// available after the live task or loom process disappears, and is replaced
/// atomically whenever the adapter advertises a refreshed control surface.
pub async fn set_acp_metadata(db: &Db, id: &str, metadata: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_acp_metadata (session_id, metadata, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT(session_id) DO UPDATE
         SET metadata = excluded.metadata, updated_at = excluded.updated_at",
    )
    .bind(id)
    .bind(metadata)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// Read the last complete provider-owned ACP composer metadata snapshot.
pub async fn get_acp_metadata(db: &Db, id: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT metadata FROM session_acp_metadata WHERE session_id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

/// Clear state owned by one ACP adapter process after setup fails. The stable
/// loom session, journal, runtime profile, and durable human prompt queue stay
/// intact so the failed session can be inspected or handed off.
pub async fn clear_acp_state(db: &Db, id: &str) -> Result<()> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    sqlx::query(
        "UPDATE sessions
         SET acp_session_id = NULL, acp_ack_seq = 0, acp_inflight = NULL,
             current_mode = NULL
         WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM session_acp_metadata WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
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

/// Provider-owned durable state retained while a handoff tears down its source.
/// A failed teardown can restore this exact snapshot under the claimed
/// generation instead of leaving a false `running` row with no provider.
#[derive(Debug, Clone)]
pub struct HandoffSourceState {
    pub status: String,
    pub acp_session_id: Option<String>,
    pub acp_ack_seq: i64,
    pub acp_inflight: Option<String>,
    pub current_mode: Option<String>,
    pub metadata: Option<String>,
}

/// Atomically claim the reviewed source generation for provider replacement
/// while preserving the source's durable provider state for fenced rollback.
pub async fn claim_handoff(
    db: &Db,
    id: &str,
    expected_mutation_revision: i64,
) -> Result<Option<HandoffSourceState>> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let query = select_sessions("WHERE id = ? AND mutation_revision = ?");
    let Some(session) = sqlx::query_as::<_, Session>(&query)
        .bind(id)
        .bind(expected_mutation_revision)
        .fetch_optional(&mut *tx)
        .await?
    else {
        tx.rollback().await?;
        return Ok(None);
    };
    let metadata =
        sqlx::query_scalar("SELECT metadata FROM session_acp_metadata WHERE session_id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let result = sqlx::query(
        "UPDATE sessions
         SET status = 'handoff',
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND mutation_revision = ?",
    )
    .bind(id)
    .bind(expected_mutation_revision)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(None);
    }
    tx.commit().await?;
    Ok(Some(HandoffSourceState {
        status: session.status,
        acp_session_id: session.acp_session_id,
        acp_ack_seq: session.acp_ack_seq,
        acp_inflight: session.acp_inflight,
        current_mode: session.current_mode,
        metadata,
    }))
}

/// Clear the superseded provider state after its supervisor is gone, while the
/// handoff still owns the claimed generation. The replacement handshake may
/// then populate fresh provider state before the final policy commit.
pub async fn clear_claimed_handoff_source(
    db: &Db,
    id: &str,
    expected_mutation_revision: i64,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let changed = sqlx::query(
        "UPDATE sessions
         SET acp_session_id = NULL, acp_ack_seq = 0, acp_inflight = NULL,
             current_mode = NULL
         WHERE id = ? AND mutation_revision = ?",
    )
    .bind(id)
    .bind(expected_mutation_revision)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !changed {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM session_acp_metadata WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Restore a source after teardown failed, only while the handoff still owns
/// its claimed generation. Returns the new generation used by any subsequent
/// error transition.
pub async fn rollback_handoff_claim(
    db: &Db,
    id: &str,
    expected_mutation_revision: i64,
    source: &HandoffSourceState,
) -> Result<Option<i64>> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let next_generation = expected_mutation_revision + 1;
    let changed = sqlx::query(
        "UPDATE sessions
         SET status = ?, acp_session_id = ?, acp_ack_seq = ?,
             acp_inflight = ?, current_mode = ?,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND mutation_revision = ?",
    )
    .bind(&source.status)
    .bind(&source.acp_session_id)
    .bind(source.acp_ack_seq)
    .bind(&source.acp_inflight)
    .bind(&source.current_mode)
    .bind(id)
    .bind(expected_mutation_revision)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !changed {
        tx.rollback().await?;
        return Ok(None);
    }
    sqlx::query("DELETE FROM session_acp_metadata WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if let Some(metadata) = &source.metadata {
        sqlx::query(
            "INSERT INTO session_acp_metadata (session_id, metadata, updated_at)
             VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(metadata)
        .bind(now_iso())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(next_generation))
}

/// Record an honest provider-less error after the source cannot be restored.
pub async fn fail_handoff_claim(
    db: &Db,
    id: &str,
    expected_mutation_revision: i64,
) -> Result<bool> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let changed = sqlx::query(
        "UPDATE sessions
         SET status = 'error', acp_session_id = NULL, acp_ack_seq = 0,
             acp_inflight = NULL, current_mode = NULL,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND mutation_revision = ?",
    )
    .bind(id)
    .bind(expected_mutation_revision)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !changed {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM session_acp_metadata WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Commit the replacement runtime policy and final lifecycle status only while
/// the handoff still owns its claimed mutation generation. Provider-private
/// state written by the new ACP task is deliberately preserved.
pub async fn prepare_handoff(
    db: &Db,
    id: &str,
    status: &str,
    policy: &SessionHandoffPolicy,
    expected_mutation_revision: i64,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE sessions
         SET agent_kind = ?, model = ?, effort = ?, status = ?,
             profile = ?, launch_mode = ?, profile_revision = ?,
             profile_lifetime = ?, policy_strict = ?, policy_env_clear = ?,
             policy_ambient_allowlist = ?,
             policy_idle_archive_secs = ?, policy_turn_budget = ?,
             policy_prelude = ?, policy_restricted = ?, policy_github_repositories = ?,
             policy_allowed_tools = ?, policy_mcp_access = ?,
             launch_snapshot = ?,
             mutation_revision = mutation_revision + 1
         WHERE id = ? AND mutation_revision = ?",
    )
    .bind(&policy.agent_kind)
    .bind(&policy.model)
    .bind(&policy.effort)
    .bind(status)
    .bind(&policy.profile)
    .bind(&policy.launch_mode)
    .bind(policy.profile_revision)
    .bind(policy.profile_lifetime)
    .bind(policy.strict)
    .bind(policy.env_clear)
    .bind(&policy.ambient_allowlist)
    .bind(policy.idle_archive_secs)
    .bind(policy.turn_budget)
    .bind(&policy.prelude)
    .bind(policy.restricted)
    .bind(&policy.github_repositories)
    .bind(&policy.allowed_tools)
    .bind(&policy.mcp_access)
    .bind(&policy.launch_snapshot)
    .bind(id)
    .bind(expected_mutation_revision)
    .execute(db)
    .await?;
    let changed = result.rows_affected() == 1;
    if changed {
        tracing::info!(
            session = %id,
            agent_kind = %policy.agent_kind,
            model = %policy.model,
            effort = %policy.effort,
            status,
            "session runtime handed off"
        );
    }
    Ok(changed)
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

pub async fn delete(db: &Db, id: &str) -> Result<Option<i64>> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let had_placement: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM session_placements placement
             JOIN sessions session ON session.id = placement.session_id
             WHERE placement.session_id = ? AND session.managed_by IS NULL
         )",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let layout_revision = if had_placement {
        bump_layout_revision_tx(&mut tx).await?;
        Some(
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&mut *tx)
                .await?,
        )
    } else {
        None
    };
    tx.commit().await?;
    tracing::info!(session = %id, "session row deleted");
    Ok(layout_revision)
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

    #[tokio::test]
    async fn persisted_rows_survive_a_replacement_process_widening_the_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weaver.db");
        let db = crate::db::connect(&path).await.unwrap();
        let branch = branch_mod::upsert(&db, "/repo", "weaver/restart", "main")
            .await
            .unwrap();
        insert(&db, &new_session("rolling", &branch.id, None))
            .await
            .unwrap();

        // Populate this process generation's prepared-statement metadata.
        branch_mod::get(&db, &branch.id).await.unwrap().unwrap();
        get(&db, "rolling").await.unwrap().unwrap();

        // A replacement loom migrates through its own SQLite connection while
        // the old server is still draining long-lived HTTP streams.
        let replacement = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}", path.display()))
            .await
            .unwrap();
        sqlx::query("ALTER TABLE branches ADD COLUMN replacement_branch_field TEXT")
            .execute(&replacement)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE sessions ADD COLUMN replacement_session_field TEXT")
            .execute(&replacement)
            .await
            .unwrap();
        replacement.close().await;

        // Explicit projections keep the old binary's row shape stable. With
        // `SELECT *`, sqlx 0.8 captures the old metadata and then panics when
        // SQLite recompiles the statement to expose the newly added column.
        assert_eq!(
            branch_mod::get(&db, &branch.id)
                .await
                .unwrap()
                .unwrap()
                .branch,
            "weaver/restart"
        );
        assert_eq!(get(&db, "rolling").await.unwrap().unwrap().id, "rolling");
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
        let visible_revision: i64 =
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        let warm = insert(&db, &new_session("warm", &warm_branch, Some("ov-1")))
            .await
            .unwrap();
        assert_eq!(warm.managed_by.as_deref(), Some("ov-1"), "marker persists");
        assert!(
            crate::session_layout::placement(&db, "warm")
                .await
                .unwrap()
                .is_none(),
            "warm infrastructure has no canonical placement"
        );
        let after_warm_insert: i64 =
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            after_warm_insert, visible_revision,
            "warm insertion does not invalidate visible layout"
        );

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
        let layout = crate::session_layout::get_layout(&db, "test")
            .await
            .unwrap();
        assert!(
            layout
                .spaces
                .iter()
                .flat_map(|space| &space.groups)
                .flat_map(|group| &group.session_ids)
                .all(|id| id != "warm"),
            "workbench layout hides managed infrastructure"
        );

        let owned = active_managed_by(&db, "ov-1").await.unwrap().unwrap();
        assert_eq!(owned.id, "warm", "the watch's warm session resolves");
        assert!(
            active_managed_by(&db, "ov-other").await.unwrap().is_none(),
            "no warm session for a watch that owns none"
        );
        delete(&db, "warm").await.unwrap();
        let after_warm_delete: i64 =
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            after_warm_delete, visible_revision,
            "warm removal does not invalidate visible layout"
        );
    }

    #[tokio::test]
    async fn placement_defaults_route_ops_and_delegated_sessions_inherit() {
        let db = crate::db::connect_in_memory().await.unwrap();

        let slack_branch = branch_id(&db, "weaver/slack-thread").await;
        let mut slack = new_session("slack-thread", &slack_branch, None);
        slack.origin = "slack".to_string();
        insert(&db, &slack).await.unwrap();
        let slack_placement = crate::session_layout::placement(&db, "slack-thread")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slack_placement.space_name, "Slack");
        assert_eq!(slack_placement.group_name, "Inbox");

        let watch_branch = branch_id(&db, "weaver/watch-result").await;
        let mut watch = new_session("watch-result", &watch_branch, None);
        watch.origin = "watch".to_string();
        watch.class = "automation".to_string();
        insert(&db, &watch).await.unwrap();
        let watch_placement = crate::session_layout::placement(&db, "watch-result")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(watch_placement.space_name, "Ops");
        assert_eq!(watch_placement.group_name, "Inbox");

        let parent_branch = branch_id(&db, "weaver/parent").await;
        insert(&db, &new_session("parent", &parent_branch, None))
            .await
            .unwrap();
        let initial = crate::session_layout::get_layout(&db, "test")
            .await
            .unwrap();
        let focused = crate::session_layout::create_group(
            &db,
            "test",
            &weaver_api::CreateSessionGroupReq {
                space_id: "space-user".to_string(),
                name: "Focused".to_string(),
                expected_revision: initial.revision,
            },
        )
        .await
        .unwrap();
        let focused_group = focused
            .spaces
            .iter()
            .flat_map(|space| &space.groups)
            .find(|group| group.name == "Focused")
            .unwrap();
        let focused_id = focused_group.id.clone();
        crate::session_layout::move_sessions(
            &db,
            "test",
            &weaver_api::MoveSessionsReq {
                session_ids: vec!["parent".to_string()],
                destination_group_id: focused_id.clone(),
                before_session_id: None,
                expected_revision: focused.revision,
            },
        )
        .await
        .unwrap();

        let child_branch = branch_id(&db, "weaver/child").await;
        let mut child = new_session("child", &child_branch, None);
        child.origin = "agent".to_string();
        child.parent_branch_id = Some(parent_branch);
        let mut policy = SessionLaunchPolicy::compatible(&child);
        policy.parent_session_id = Some("parent".to_string());
        insert_with_policy(&db, &child, &policy).await.unwrap();
        assert_eq!(
            crate::session_layout::placement(&db, "child")
                .await
                .unwrap()
                .unwrap()
                .group_id,
            focused_id
        );

        let mut revision = crate::session_layout::get_layout(&db, "test")
            .await
            .unwrap()
            .revision;
        for (kind, value, group_id) in [
            (
                weaver_api::SessionPlacementSelectorKind::Profile,
                "default",
                "group-github-inbox",
            ),
            (
                weaver_api::SessionPlacementSelectorKind::Watch,
                "watch-1",
                focused_id.as_str(),
            ),
        ] {
            revision = crate::session_layout::set_default(
                &db,
                "test",
                &weaver_api::SetSessionPlacementDefaultReq {
                    selector_kind: kind,
                    selector_value: value.to_string(),
                    group_id: group_id.to_string(),
                    expected_revision: revision,
                },
            )
            .await
            .unwrap()
            .revision;
        }
        sqlx::query(
            "INSERT INTO automation_runs
             (id, actor_subject, source, profile, idempotency_key, request_json,
              session_id, status, created_at, updated_at)
             VALUES ('run-watch', 'watch-1', 'ops', 'default', 'placement',
              '{\"watch_id\":\"watch-1\"}', 'reserved', 'creating', '', '')",
        )
        .execute(&db)
        .await
        .unwrap();
        let run_view: weaver_api::RunView = crate::runs::get(&db, "run-watch")
            .await
            .unwrap()
            .unwrap()
            .into();
        assert_eq!(run_view.watch_id.as_deref(), Some("watch-1"));

        for (id, expected, remove) in [
            (
                "by-watch",
                focused_id.as_str(),
                Some((weaver_api::SessionPlacementSelectorKind::Watch, "watch-1")),
            ),
            (
                "by-profile",
                "group-github-inbox",
                Some((weaver_api::SessionPlacementSelectorKind::Profile, "default")),
            ),
            ("by-origin", "group-ops-inbox", None),
        ] {
            let branch = branch_id(&db, &format!("weaver/{id}")).await;
            let mut launched = new_session(id, &branch, None);
            launched.origin = "actions".to_string();
            let mut policy = SessionLaunchPolicy::compatible(&launched);
            policy.automation_run_id = Some("run-watch".to_string());
            insert_with_policy(&db, &launched, &policy).await.unwrap();
            assert_eq!(
                crate::session_layout::placement(&db, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .group_id,
                expected
            );
            if let Some((kind, value)) = remove {
                let revision = crate::session_layout::get_layout(&db, "test")
                    .await
                    .unwrap()
                    .revision;
                crate::session_layout::delete_default(&db, "test", kind, value, revision)
                    .await
                    .unwrap();
            }
        }
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
    async fn lifecycle_transition_has_single_owner_and_atomic_completion() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/transition-owner").await;
        insert(&db, &new_session("transition-owner", &branch, None))
            .await
            .unwrap();

        assert!(
            begin_transition(&db, "transition-owner", "archiving", "Stopping agent")
                .await
                .unwrap()
        );
        assert!(
            !begin_transition(&db, "transition-owner", "adopting", "Preparing adoption")
                .await
                .unwrap()
        );
        assert!(
            update_transition_step(&db, "transition-owner", "archiving", "Removing worktree")
                .await
                .unwrap()
        );

        let active = get(&db, "transition-owner").await.unwrap().unwrap();
        assert_eq!(active.status, "running");
        assert_eq!(active.lifecycle_transition.as_deref(), Some("archiving"));
        assert_eq!(active.lifecycle_step.as_deref(), Some("Removing worktree"));

        assert!(
            complete_transition(&db, "transition-owner", "archiving", "archived")
                .await
                .unwrap()
        );
        let archived = get(&db, "transition-owner").await.unwrap().unwrap();
        assert_eq!(archived.status, "archived");
        assert!(archived.lifecycle_transition.is_none());
        assert!(archived.lifecycle_step.is_none());
        assert!(archived.lifecycle_transition_started_at.is_none());
        assert!(archived.lifecycle_transition_owner_pid.is_none());
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
    async fn orphan_transition_does_not_interrupt_a_handoff() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/handoff-orphan-race").await;
        let mut session = new_session("handoff-orphan-race", &branch, None);
        session.status = "handoff".to_string();
        insert(&db, &session).await.unwrap();

        assert!(!mark_orphaned(&db, &session.id).await.unwrap());
        assert_eq!(
            get(&db, &session.id).await.unwrap().unwrap().status,
            "handoff"
        );
    }

    #[tokio::test]
    async fn recovery_claim_is_compare_and_set() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/recover-claim").await;
        let mut archived = new_session("recover-claim", &branch, None);
        archived.status = "archived".to_string();
        insert(&db, &archived).await.unwrap();

        assert!(claim_recovery(&db, "recover-claim").await.unwrap());
        assert_eq!(
            get(&db, "recover-claim").await.unwrap().unwrap().status,
            "created"
        );
        assert!(
            !claim_recovery(&db, "recover-claim").await.unwrap(),
            "a second recovery cannot claim the same archived row"
        );
    }

    #[tokio::test]
    async fn recovery_claim_loses_to_an_active_session_on_the_same_branch() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/recover-slot").await;
        let mut archived = new_session("recover-slot-old", &branch, None);
        archived.status = "archived".to_string();
        insert(&db, &archived).await.unwrap();
        insert(&db, &new_session("recover-slot-live", &branch, None))
            .await
            .unwrap();

        assert!(
            claim_recovery(&db, "recover-slot-old").await.is_err(),
            "the unique active-branch index arbitrates the recovery claim"
        );
        assert_eq!(
            get(&db, "recover-slot-old").await.unwrap().unwrap().status,
            "archived",
            "a failed claim leaves the archived row unchanged"
        );
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

        let policy = SessionHandoffPolicy {
            agent_kind: "codex".to_string(),
            model: "gpt-5.4".to_string(),
            effort: "high".to_string(),
            profile: "default".to_string(),
            launch_mode: "auto".to_string(),
            profile_revision: 1,
            profile_lifetime: 1,
            strict: false,
            env_clear: false,
            ambient_allowlist: "[]".to_string(),
            idle_archive_secs: None,
            turn_budget: 0,
            prelude: "weaver".to_string(),
            restricted: false,
            github_repositories: "[]".to_string(),
            allowed_tools: "[]".to_string(),
            mcp_access: r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[]}"#
                .to_string(),
            launch_snapshot: r#"{"agent":"codex"}"#.to_string(),
        };
        assert!(claim_handoff(&db, "handoff", 1).await.unwrap().is_some());
        assert!(clear_claimed_handoff_source(&db, "handoff", 2)
            .await
            .unwrap());
        assert!(prepare_handoff(&db, "handoff", "running", &policy, 2)
            .await
            .unwrap());
        let session = get(&db, "handoff").await.unwrap().unwrap();
        assert_eq!(session.agent_kind, "codex");
        assert_eq!(session.model, "gpt-5.4");
        assert_eq!(session.effort, "high");
        assert_eq!(session.status, "running");
        assert_eq!(session.launch_snapshot, r#"{"agent":"codex"}"#);
        assert!(session.acp_session_id.is_none());
        assert_eq!(session.acp_ack_seq, 0);
        assert!(session.acp_inflight.is_none());
        assert!(session.current_mode.is_none());
        assert_eq!(session.pending_prompt.as_deref(), Some("queued"));
        assert_eq!(session.mutation_revision, 3);
    }

    #[tokio::test]
    async fn handoff_final_commit_loses_to_newer_status_generation() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_id(&db, "weaver/handoff-cas").await;
        let mut input = new_session("handoff-cas", &branch, None);
        input.agent_kind = "claude".to_string();
        input.protocol = "acp".to_string();
        insert(&db, &input).await.unwrap();
        assert!(claim_handoff(&db, "handoff-cas", 1)
            .await
            .unwrap()
            .is_some());
        set_status(&db, "handoff-cas", "done").await.unwrap();

        let policy = SessionHandoffPolicy {
            agent_kind: "codex".to_string(),
            model: "gpt-5.6".to_string(),
            effort: "high".to_string(),
            profile: "replacement".to_string(),
            launch_mode: "plan".to_string(),
            profile_revision: 9,
            profile_lifetime: 4,
            strict: true,
            env_clear: true,
            ambient_allowlist: "[]".to_string(),
            idle_archive_secs: None,
            turn_budget: 0,
            prelude: "none".to_string(),
            restricted: false,
            github_repositories: "[]".to_string(),
            allowed_tools: "[]".to_string(),
            mcp_access: r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[]}"#
                .to_string(),
            launch_snapshot: r#"{"agent":"codex"}"#.to_string(),
        };
        assert!(!prepare_handoff(&db, "handoff-cas", "running", &policy, 2)
            .await
            .unwrap());
        let session = get(&db, "handoff-cas").await.unwrap().unwrap();
        assert_eq!(session.status, "done");
        assert_eq!(session.agent_kind, "claude");
        assert_eq!(session.profile, crate::agent_kind::DEFAULT_PROFILE);
        assert_eq!(session.mutation_revision, 3);
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
