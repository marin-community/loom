//! axum REST API + SSE. The Vue SPA is the primary consumer.
//!
//! Endpoint layout (post phase-4 rename):
//!
//! * `/api/sessions` — list + create active sessions (each session is one
//!   tmux + one agent attached to a branch).
//! * `/api/sessions/{id}` — GET / PATCH / DELETE a single session, plus the
//!   action subroutes `/archive`, `/adopt`,
//!   `/log`, `/events`, and `/terminal` (a WebSocket bridged to the session's
//!   tmux via a PTY — see `crate::terminal`). Interacting with the agent
//!   (keystrokes, keys, TUIs) happens entirely over `/terminal`.
//! * `/api/branches` — list every tracked branch (with or without an active
//!   session). `/api/branches/{id}` — GET / PATCH (goal / title / description).
//! * `/api/branches/{id}/issues` — list / POST issues for a branch.
//! * `/api/issues/{id}` — GET / PATCH / DELETE an issue by id.
//! * `/api/repos/recent`, `/api/repos/branches`, `/api/health`, `/api/settings`
//!   — unchanged.
//!
//! The `/api/hook` endpoint that used to exist is gone — agent hooks now go
//! through `weaver hook --event …` which writes an `events` row consumed by
//! the monitor loop.
//!
//! ### SessionView payload
//!
//! The session-scoped endpoints return a `SessionView` shaped like:
//!
//! ```json
//! {
//!   "id": "<session id>",
//!   "status": "running",            // lifecycle: created|launching|running|orphaned|done|error
//!   "work_dir": "/path/to/.worktrees/foo",
//!   "tmux_session": "weaver-abcd1234",
//!   "agent_kind": "claude",
//!   "github_repo": null,
//!   "last_activity_at": "...",
//!   "created_at": "...",
//!   "updated_at": "...",
//!   "branch": {
//!     "id": "<branch id>",
//!     "name": "feature-x",            // short label (weaver/<slug> with prefix stripped)
//!     "title": "...",
//!     "goal": "...",
//!     "description": "...",         // current-state message (weaver set-status)
//!     "attention": "ok",            // agent-declared: ok|attention|blocked
//!     "repo_root": "/path/to/repo",
//!     "branch": "weaver/feature-x",
//!     "base_branch": "main",
//!     "created_at": "...",
//!     "updated_at": "...",
//!     "open_issue_count": 0
//!   }
//! }
//! ```

use std::convert::Infallible;
use std::path::{Component, PathBuf};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{self, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::db::Db;
use crate::events::{Event, EventBus};
use crate::session::{self as session_mod, NewSession, Session};
use crate::{agent, config, db, events, git, github, repo, tmux};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::issue::Issue;
use weaver_core::{plan, repo_config};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub bus: EventBus,
    /// host:port the server is bound to, used to build child-process env.
    pub addr: String,
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: String,
    details: Option<Value>,
}

impl AppError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(status = %self.status.as_u16(), message = %self.message, "request failed");
        } else {
            tracing::warn!(status = %self.status.as_u16(), message = %self.message, "request rejected");
        }
        let mut body = json!({ "error": self.message });
        if let Some(details) = self.details {
            body["details"] = details;
        }
        (self.status, Json(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.into().to_string())
    }
}

type ApiResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------
// View payloads
// ---------------------------------------------------------------------------

/// Branch with denormalized open-issue count, returned by `/api/branches` and
/// embedded under `SessionView::branch`.
#[derive(Debug, Clone, Serialize)]
pub struct BranchView {
    pub id: String,
    /// Short label: the branch name with the optional `weaver/` prefix stripped.
    pub name: String,
    pub title: String,
    pub goal: String,
    /// The agent's current-state message, set with `attention` via
    /// `weaver set-status`.
    pub description: String,
    /// Agent-declared attention level (`ok` | `attention` | `blocked`) — the
    /// "does this need me?" signal the dashboard filters on. The accompanying
    /// message is `description`.
    pub attention: String,
    pub repo_root: String,
    pub branch: String,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub open_issue_count: i64,
    /// The branch's latest GitHub pull-request snapshot (link, review decision,
    /// check rollup), or `null` when GitHub polling is off, the repo has no
    /// remote PR, or `gh` is unavailable. Maintained by the poll loop in
    /// [`crate::github`].
    pub github: Option<github::GithubStatus>,
}

impl BranchView {
    async fn build(db: &Db, branch: &Branch) -> ApiResult<Self> {
        // The badge counts the work this branch has claimed, not the whole repo.
        let open = weaver_core::issue::open_count_for_branch(db, &branch.repo_root, &branch.branch)
            .await
            .unwrap_or(0);
        // Best-effort: a missing/erroring snapshot just renders as no GitHub info.
        let github = github::get_status(db, &branch.id).await.ok().flatten();
        Ok(Self::from_parts(branch, open, github))
    }

    fn from_parts(
        branch: &Branch,
        open_issue_count: i64,
        github: Option<github::GithubStatus>,
    ) -> Self {
        let name = branch
            .branch
            .strip_prefix("weaver/")
            .unwrap_or(&branch.branch)
            .to_string();
        BranchView {
            id: branch.id.clone(),
            name,
            title: branch.title.clone(),
            goal: branch.goal.clone(),
            description: branch.description.clone(),
            attention: branch.attention.clone(),
            repo_root: branch.repo_root.clone(),
            branch: branch.branch.clone(),
            base_branch: branch.base_branch.clone(),
            created_at: branch.created_at.clone(),
            updated_at: branch.updated_at.clone(),
            open_issue_count,
            github,
        }
    }
}

/// Session-scoped view returned by the `/api/sessions[/...]` endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub id: String,
    pub status: String,
    pub work_dir: String,
    pub tmux_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub github_repo: Option<String>,
    pub last_activity_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Branch id of the session that **launched** this one — the parent in the
    /// dashboard's session tree — or `null` for a top-level session. Stamped on
    /// the session row at launch from the resolved launcher (`parent_branch`), so
    /// reads need no extra query and the link can't drift. The dashboard groups
    /// the list into threads by it; a child whose parent is absent (archived, or
    /// never tracked) renders at the top level.
    pub parent_id: Option<String>,
    /// The tracking issue opened for this session's task at launch (the handle
    /// handed back to whoever launched it). Only populated on the create
    /// response; `None` on the list/get/patch paths, which don't recompute it.
    pub tracking_issue: Option<i64>,
    pub branch: BranchView,
}

impl SessionView {
    async fn build(db: &Db, session: &Session, branch: &Branch) -> ApiResult<Self> {
        let bv = BranchView::build(db, branch).await?;
        Ok(SessionView {
            id: session.id.clone(),
            status: session.status.clone(),
            work_dir: session.work_dir.clone(),
            tmux_session: session.tmux_session.clone(),
            agent_kind: session.agent_kind.clone(),
            model: session.model.clone(),
            effort: session.effort.clone(),
            github_repo: session.github_repo.clone(),
            last_activity_at: session
                .last_activity_at
                .clone()
                .unwrap_or_else(|| branch.updated_at.clone()),
            created_at: session.created_at.clone(),
            updated_at: branch.updated_at.clone(),
            parent_id: session.parent_branch_id.clone(),
            tracking_issue: None,
            branch: bv,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueView {
    pub id: i64,
    pub repo_root: String,
    pub github_repo: Option<String>,
    /// Branch the issue was created from (provenance).
    pub source_branch: Option<String>,
    /// Branch currently working it; `null` is the unclaimed repo backlog.
    pub claimed_branch: Option<String>,
    pub title: String,
    pub body: String,
    pub status: String,
    pub github_issue: Option<i64>,
    /// Link to a plan task (`"<slug>#T3"`) when materialized from a plan.
    pub plan_task: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

impl From<Issue> for IssueView {
    fn from(i: Issue) -> Self {
        IssueView {
            id: i.id,
            repo_root: i.repo_root,
            github_repo: i.github_repo,
            source_branch: i.source_branch,
            claimed_branch: i.claimed_branch,
            title: i.title,
            body: i.body,
            status: i.status,
            github_issue: i.github_issue,
            plan_task: i.plan_task,
            created_at: i.created_at,
            updated_at: i.updated_at,
            closed_at: i.closed_at,
        }
    }
}

/// Resolve a session key (session id, branch id, branch name, or `repo:branch`)
/// to `(Session, Branch)`. The session must exist and be active; clients hitting
/// a branch with no live session get a 404.
pub async fn require_session(db: &Db, key: &str) -> ApiResult<(Session, Branch)> {
    if let Some((session, branch)) = session_mod::with_branch(db, key).await? {
        return Ok((session, branch));
    }
    if let Some(branch) = branch_mod::resolve_key(db, key).await? {
        if let Some(session) = session_mod::active_for_branch(db, &branch.id).await? {
            return Ok((session, branch));
        }
    }
    Err(AppError::not_found("session"))
}

async fn require_branch(db: &Db, key: &str) -> ApiResult<Branch> {
    if let Some(branch) = branch_mod::resolve_key(db, key).await? {
        return Ok(branch);
    }
    if let Some((_, branch)) = session_mod::with_branch(db, key).await? {
        return Ok(branch);
    }
    Err(AppError::not_found("branch"))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn static_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_STATIC_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("static")
        .join("dist")
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        // Sessions
        .route("/sessions", get(list_sessions).post(create_session))
        .route(
            "/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route("/sessions/{id}/archive", post(archive_session))
        .route("/sessions/{id}/adopt", post(adopt_session))
        .route("/sessions/{id}/github", post(refresh_github_session))
        .route("/sessions/{id}/tree", get(tree_session))
        .route("/sessions/{id}/file", get(file_session).put(write_file))
        .route("/sessions/{id}/raw", get(raw_session))
        .route("/sessions/{id}/plan", get(get_plan))
        .route("/sessions/{id}/plan/sync", post(sync_plan))
        .route(
            "/sessions/{id}/scratch",
            get(list_scratch)
                .post(upload_scratch)
                .delete(delete_scratch),
        )
        .route("/sessions/{id}/log", get(log_session))
        .route("/sessions/{id}/events", get(events_sse))
        .route("/sessions/{id}/terminal", get(crate::terminal::terminal_ws))
        // Drive a session's tmux pane: type a message, interrupt, peek at it.
        .route("/sessions/{id}/send", post(send_session))
        .route("/sessions/{id}/interrupt", post(interrupt_session))
        .route("/sessions/{id}/preview", get(preview_session))
        // Branches & issues
        .route("/branches", get(list_branches))
        .route("/branches/{id}", get(get_branch).patch(patch_branch))
        .route(
            "/branches/{id}/issues",
            get(list_branch_issues).post(create_branch_issue),
        )
        .route(
            "/issues/{id}",
            get(get_issue).patch(patch_issue).delete(delete_issue),
        )
        // Misc
        .route("/repos/recent", get(recent_repos))
        .route("/repos/branches", get(repo_branches))
        .route(
            "/repos/issues",
            get(list_repo_issues).post(create_repo_issue),
        )
        .route("/settings", get(get_settings).patch(patch_settings))
        // Scratch uploads can carry images / logs; lift the default 2 MB cap.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let index = static_dir().join("index.html");
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(static_dir()).fallback(ServeFile::new(index)))
        .layer(CorsLayer::permissive())
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

async fn list_sessions(State(st): State<AppState>) -> ApiResult<Json<Vec<SessionView>>> {
    let sessions = session_mod::list(&st.db).await?;
    let mut views: Vec<SessionView> = Vec::with_capacity(sessions.len());
    for s in sessions {
        if let Some(branch) = branch_mod::get(&st.db, &s.branch_id).await? {
            views.push(SessionView::build(&st.db, &s, &branch).await?);
        }
    }
    Ok(Json(views))
}

async fn get_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(SessionView::build(&st.db, &session, &branch).await?))
}

#[derive(Debug, Deserialize)]
struct CreateReq {
    cwd: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    issue: Option<i64>,
    /// A pre-existing weaver issue id to claim for this session (fan-out
    /// pickup). Seeds title/goal/description and stamps `claimed_branch`.
    #[serde(default)]
    claim_issue: Option<i64>,
    #[serde(default)]
    existing_branch: Option<String>,
    /// The branch (id or name) of the agent launching this session, when it is
    /// itself a weaver session delegating work. Recorded as the tracking
    /// issue's `source_branch` so the parent's sub-trees are attributable. The
    /// `loom` CLI fills this from `$WEAVER_BRANCH`; a human/dashboard launch
    /// leaves it unset.
    #[serde(default)]
    parent_branch: Option<String>,
    /// Model tier ('haiku' | 'sonnet' | 'opus'); blank/absent inherits the
    /// configured `agent.claude_args`.
    #[serde(default)]
    model: Option<String>,
    /// Reasoning effort ('low' | 'medium' | 'high' | 'xhigh' | 'max');
    /// blank/absent inherits the configured `agent.claude_args`.
    #[serde(default)]
    effort: Option<String>,
    /// Reference files to drop into the new worktree's `scratch/` directory
    /// before the agent launches. The agent is told they are there (see
    /// `write_initial_scratch`). Empty/absent for a plain session.
    #[serde(default)]
    scratch: Vec<ScratchUpload>,
}

/// One launch-time scratch file: a name plus its base64-encoded bytes. JSON
/// can't carry raw binary, so the UI reads each dropped file as base64.
#[derive(Debug, Deserialize)]
struct ScratchUpload {
    name: String,
    #[serde(default)]
    content_base64: String,
}

async fn create_session(
    State(st): State<AppState>,
    Json(req): Json<CreateReq>,
) -> ApiResult<Json<SessionView>> {
    let cwd = PathBuf::from(&req.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    // Canonicalize so repo identity matches the `weaver` CLI's resolver — issues
    // are keyed on this path and the two binaries must agree on it.
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);

    let agent = match req.agent {
        Some(a) => a,
        None => config::get_or(&st.db, "agent.default", config::DEFAULT_AGENT).await,
    };

    // Normalize and validate the model / effort selections. Blank means
    // "inherit the configured default"; anything non-blank must be known.
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !model.is_empty() && !agent::MODELS.contains(&model.as_str()) {
        return Err(AppError::bad_request(format!(
            "unknown model '{model}' — expected one of {}",
            agent::MODELS.join(", ")
        )));
    }
    let effort = req
        .effort
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !effort.is_empty() && !agent::EFFORTS.contains(&effort.as_str()) {
        return Err(AppError::bad_request(format!(
            "unknown effort '{effort}' — expected one of {}",
            agent::EFFORTS.join(", ")
        )));
    }

    // Build title/goal/description; an optional GitHub issue seeds all three.
    let mut goal = req.goal.unwrap_or_default().trim().to_string();
    let mut title = req
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let mut description = String::new();
    let mut github_repo = None;
    let mut github_issue: Option<i64> = None;
    if let Some(number) = req.issue {
        let issue = github::fetch_issue(&repo_root, number)
            .await
            .map_err(|e| AppError::bad_request(format!("issue #{number}: {e}")))?;
        if title.is_none() {
            title = Some(issue.title.clone());
        }
        if goal.is_empty() {
            goal = if issue.body.trim().is_empty() {
                issue.title.clone()
            } else {
                format!("{}\n\n{}", issue.title, issue.body)
            };
        }
        description = issue.body.clone();
        github_issue = Some(number);
        github_repo = github::repo_slug(&repo_root).await.ok();
    }

    // Claiming an existing weaver issue seeds the same three fields from it.
    let repo_root_str = repo_root.display().to_string();
    let mut claimed_issue_id: Option<i64> = None;
    if let Some(issue_id) = req.claim_issue {
        let issue = weaver_core::issue::get(&st.db, issue_id)
            .await?
            .ok_or_else(|| AppError::not_found("issue"))?;
        if issue.repo_root != repo_root_str {
            return Err(AppError::bad_request(format!(
                "issue #{issue_id} belongs to a different repo"
            )));
        }
        if title.is_none() {
            title = Some(issue.title.clone());
        }
        if goal.is_empty() {
            goal = if issue.body.trim().is_empty() {
                issue.title.clone()
            } else {
                format!("{}\n\n{}", issue.title, issue.body)
            };
        }
        if description.is_empty() {
            description = issue.body.clone();
        }
        claimed_issue_id = Some(issue_id);
    }
    let title = title.unwrap_or_else(|| {
        if goal.is_empty() {
            "Untitled session".to_string()
        } else {
            branch_mod::derive_title(&goal)
        }
    });

    let existing = req
        .existing_branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    if existing.is_some()
        && req
            .name
            .as_deref()
            .map(str::trim)
            .is_some_and(|n| !n.is_empty())
    {
        return Err(AppError::bad_request(
            "`name` and `existing_branch` are mutually exclusive",
        ));
    }

    // Unless the caller pins a base, fork from a freshly-fetched `origin/<default
    // branch>` so new work starts from the latest mainline, not the launching
    // checkout's (possibly stale) current branch. `default_base` degrades to the
    // current branch on a remote-less repo.
    let base = match req.base.clone() {
        Some(b) => b,
        None => git::default_base(&repo_root).await?,
    };

    let (branch_name, work_dir) = if let Some(existing_branch) = existing {
        if !git::branch_exists(&repo_root, existing_branch).await {
            return Err(AppError::bad_request(format!(
                "branch '{existing_branch}' does not exist in this repo"
            )));
        }
        // Reject if a tracked branch already has a live session.
        if let Some(existing_b) =
            branch_mod::find_by_repo_branch(&st.db, &repo_root_str, existing_branch).await?
        {
            if session_mod::active_for_branch(&st.db, &existing_b.id)
                .await?
                .is_some()
            {
                return Err(AppError::conflict(format!(
                    "branch '{existing_branch}' already has an active session"
                )));
            }
        }
        let work_dir = match git::worktree_for_branch(&repo_root, existing_branch)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?
        {
            Some(p) => p,
            None => {
                let slug = branch_mod::slugify(existing_branch);
                let dir = repo_root.join(".worktrees").join(&slug);
                tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
                git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
                git::worktree_add_existing(&repo_root, &dir, existing_branch)
                    .await
                    .map_err(|e| AppError::bad_request(e.to_string()))?;
                dir
            }
        };
        (existing_branch.to_string(), work_dir)
    } else {
        // Create `weaver/<slug>` with a unique suffix.
        let explicit = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let base_slug = branch_mod::slugify(explicit.unwrap_or(title.as_str()));
        let mut slug = base_slug.clone();
        let mut suffix = 2;
        loop {
            let branch_name = format!("weaver/{slug}");
            let dir = repo_root.join(".worktrees").join(&slug);
            if !git::branch_exists(&repo_root, &branch_name).await && !dir.exists() {
                break;
            }
            if explicit.is_some() {
                return Err(AppError::conflict(format!(
                    "a session named '{slug}' already exists — choose a different name"
                )));
            }
            slug = format!("{base_slug}-{suffix}");
            suffix += 1;
        }
        let branch_name = format!("weaver/{slug}");
        let work_dir = repo_root.join(".worktrees").join(&slug);
        tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
        git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
        git::worktree_add(&repo_root, &work_dir, &branch_name, &base)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        (branch_name, work_dir)
    };

    // Get-or-create the branch row, then stamp its title/goal/description.
    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    branch_mod::set_title(&st.db, &branch.id, &title).await?;
    if !goal.is_empty() {
        branch_mod::set_goal(&st.db, &branch.id, &goal).await?;
    }
    if !description.is_empty() {
        branch_mod::set_description(&st.db, &branch.id, &description).await?;
    }
    // Re-fetch so the view we return reflects the freshly-stamped fields.
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "branch vanished"))?;

    // Resolve the launching parent once: it names the tracking issue's
    // `source_branch` *and* the session's tree parent (`parent_branch_id`).
    // Only attribute to a parent in *this* repo, and never to the branch itself
    // — `resolve_key` searches globally, so a stray `$WEAVER_BRANCH` from a
    // checkout elsewhere must not misattribute the link to an unrelated branch.
    let parent = match req
        .parent_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(key) => branch_mod::resolve_key(&st.db, key)
            .await?
            .filter(|b| b.repo_root == branch.repo_root && b.branch != branch.branch),
        None => None,
    };
    let parent_branch_name = parent.as_ref().map(|b| b.branch.clone());

    // Open this session's tracking issue before the launch prompt is written,
    // so the agent can be told its issue number. When an agent delegated this
    // work (`parent_branch`), the parent becomes the issue's `source_branch`.
    let tracking_issue = create_tracking_issue(
        &st,
        &branch,
        parent_branch_name.as_deref(),
        &title,
        &goal,
        &description,
        github_repo.as_deref(),
        github_issue,
        claimed_issue_id,
    )
    .await?;

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    // Drop any attached reference files into the worktree before the agent
    // launches, then tell the agent they are there. The branch goal stays the
    // clean text the user typed; the scratch and tracking notes ride on the
    // launch prompt (goal.txt) only, so they reach the agent without cluttering
    // the dashboard.
    let scratch_names = write_initial_scratch(&work_dir, &req.scratch).await?;
    let mut prompt_parts: Vec<String> = Vec::new();
    if !goal.is_empty() {
        prompt_parts.push(goal.clone());
    }
    if let Some(note) = scratch_note(&scratch_names) {
        prompt_parts.push(note);
    }
    if let Some(id) = tracking_issue {
        prompt_parts.push(tracking_note(id));
    }
    let launch_prompt = prompt_parts.join("\n\n");
    let goal_file = if launch_prompt.is_empty() {
        None
    } else {
        let f = run_dir.join("goal.txt");
        tokio::fs::write(&f, &launch_prompt).await?;
        Some(f)
    };

    let tmux_session = format!("weaver-{session_id}");
    let base_args = config::get_or(&st.db, "agent.claude_args", "").await;
    let claude_args = agent::combine_args(&base_args, &model, &effort);
    agent::launch(
        &agent::LaunchSpec {
            branch_id: &branch.id,
            agent_kind: &agent,
            work_dir: &work_dir,
            tmux_session: &tmux_session,
            goal_file: goal_file.as_deref(),
            server_addr: &st.addr,
            claude_args: &claude_args,
        },
        agent::LaunchMode::Fresh,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let status = if matches!(agent.as_str(), "shell" | "none") {
        "running"
    } else {
        "launching"
    };
    let session = session_mod::insert(
        &st.db,
        &NewSession {
            id: session_id.clone(),
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            tmux_session,
            agent_kind: agent,
            model,
            effort,
            status: status.to_string(),
            github_repo: github_repo.clone(),
            parent_branch_id: parent.as_ref().map(|b| b.id.clone()),
        },
    )
    .await?;

    if let Err(e) = repo::record_use(&st.db, &branch.repo_root).await {
        tracing::warn!(branch = %branch.id, error = %e, "failed to record recent repo");
    }
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": "session created" }),
    )
    .await
    .ok();
    tracing::info!(
        branch = %branch.id,
        session = %session.id,
        status = %session.status,
        agent = %session.agent_kind,
        "session created"
    );

    let mut view = SessionView::build(&st.db, &session, &branch).await?;
    view.tracking_issue = tracking_issue;
    Ok(Json(view))
}

/// A line appended to a session's launch prompt telling the agent which weaver
/// issue tracks its task, so it keeps the issue up to date and closes it when
/// done. Mirrors [`scratch_note`]: it rides on the prompt only, never the
/// stored goal.
fn tracking_note(issue_id: i64) -> String {
    format!(
        "This session is tracked as weaver issue #{issue_id}. Keep your status \
         current with `weaver set-status <level> \"<message>\"` as you work, and \
         run `weaver issue close {issue_id}` once the task is complete (e.g. the \
         PR is open) so whoever launched you knows you are done."
    )
}

/// Open (or adopt) the tracking issue for a freshly-launched session: the one
/// issue, claimed by the new branch, that represents its task. Whoever launched
/// the session follows progress through it.
///
/// `--claim <id>` and `--issue <n>` (GitHub) reuse the issue they already
/// imply, so a launch never opens a duplicate; a plain launch opens a fresh one
/// from the task. An empty worktree with no task at all is untracked (`None`).
/// `source_branch` records provenance — the parent branch when an agent
/// delegated this work, else the new branch itself.
#[allow(clippy::too_many_arguments)]
async fn create_tracking_issue(
    st: &AppState,
    branch: &Branch,
    parent_branch: Option<&str>,
    title: &str,
    goal: &str,
    description: &str,
    github_repo: Option<&str>,
    github_issue: Option<i64>,
    claim_issue: Option<i64>,
) -> ApiResult<Option<i64>> {
    let source = parent_branch.unwrap_or(&branch.branch).to_string();

    // Claiming an existing weaver issue: that issue *is* the tracker, so the
    // claim must actually land — otherwise we'd hand back a tracking id for an
    // issue this branch never claimed. Propagate failures rather than swallow.
    if let Some(id) = claim_issue {
        weaver_core::issue::set_claim(&st.db, id, Some(&branch.branch)).await?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "issue_claimed",
            json!({ "id": id }),
        )
        .await
        .ok();
        return Ok(Some(id));
    }

    // A GitHub-seeded launch tracks the imported issue row.
    if let Some(number) = github_issue {
        let issue = weaver_core::issue::add(
            &st.db,
            &weaver_core::issue::NewIssue {
                repo_root: branch.repo_root.clone(),
                github_repo: github_repo.map(str::to_string),
                source_branch: Some(source),
                claimed_branch: Some(branch.branch.clone()),
                title: title.to_string(),
                body: description.to_string(),
                github_issue: Some(number),
                plan_task: None,
            },
        )
        .await?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "issue_added",
            json!({ "id": issue.id, "title": issue.title }),
        )
        .await
        .ok();
        return Ok(Some(issue.id));
    }

    // No task to track (e.g. an empty `--agent shell` worktree).
    if goal.trim().is_empty() {
        return Ok(None);
    }

    let body = if description.trim().is_empty() {
        goal
    } else {
        description
    };
    let issue = weaver_core::issue::add(
        &st.db,
        &weaver_core::issue::NewIssue {
            repo_root: branch.repo_root.clone(),
            source_branch: Some(source),
            claimed_branch: Some(branch.branch.clone()),
            title: title.to_string(),
            body: body.to_string(),
            ..Default::default()
        },
    )
    .await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "issue_added",
        json!({ "id": issue.id, "title": issue.title }),
    )
    .await
    .ok();
    Ok(Some(issue.id))
}

#[derive(Debug, Deserialize)]
struct PatchSessionReq {
    status: Option<String>,
    // Branch-level fields kept here for backwards-friendly clients (the SPA
    // patches goal/title/description on the session and we forward them to
    // the underlying branch row).
    title: Option<String>,
    goal: Option<String>,
    /// The agent's current-state message — the note shown beside the level.
    /// Set together with `attention` via `weaver set-status`; the dashboard's
    /// status editor patches both.
    description: Option<String>,
    /// Agent-declared attention level (`ok` | `attention` | `blocked`).
    /// Branch-level; lets the dashboard set what the agent set via
    /// `weaver set-status`.
    attention: Option<String>,
}

/// Apply an attention-level patch to a branch: validate it, update the branch,
/// and broadcast an `attention` event. No-op when no level is supplied. The
/// accompanying message lives in `description` and is patched separately.
async fn apply_attention_patch(
    st: &AppState,
    branch: &Branch,
    level: Option<&str>,
) -> ApiResult<()> {
    let Some(level) = level else {
        return Ok(());
    };
    let level = level.trim().to_ascii_lowercase();
    if !branch_mod::is_valid_attention(&level) {
        return Err(AppError::bad_request(format!(
            "invalid attention '{level}' — expected one of {}",
            branch_mod::ATTENTION_LEVELS.join(", ")
        )));
    }
    branch_mod::set_attention(&st.db, &branch.id, &level).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "attention",
        json!({ "level": level, "source": "manual" }),
    )
    .await
    .ok();
    Ok(())
}

async fn patch_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchSessionReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    apply_attention_patch(&st, &branch, req.attention.as_deref()).await?;
    if let Some(title) = &req.title {
        branch_mod::set_title(&st.db, &branch.id, title).await?;
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal).await?;
        tokio::fs::write(db::run_dir(&session.id).join("goal.txt"), goal)
            .await
            .ok();
    }
    if let Some(description) = &req.description {
        branch_mod::set_description(&st.db, &branch.id, description).await?;
    }
    if let Some(status) = &req.status {
        if !session_mod::STATUSES.contains(&status.as_str()) {
            return Err(AppError::bad_request(format!("invalid status '{status}'")));
        }
        session_mod::set_status(&st.db, &session.id, status).await?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "status",
            json!({ "status": status, "source": "manual" }),
        )
        .await
        .ok();
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(SessionView::build(&st.db, &session, &branch).await?))
}

#[derive(Debug, Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    keep_branch: bool,
}

async fn delete_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let mut warnings: Vec<String> = Vec::new();

    tmux::kill_session(&session.tmux_session).await.ok();
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
        warnings.push(format!("worktree remove: {e}"));
        tokio::fs::remove_dir_all(&work_dir).await.ok();
    }
    if !q.keep_branch {
        if let Err(e) = git::delete_branch(&repo_root, &branch.branch).await {
            warnings.push(format!("delete branch: {e}"));
        }
    }
    tokio::fs::remove_dir_all(db::run_dir(&session.id))
        .await
        .ok();
    session_mod::delete(&st.db, &session.id).await?;
    // Release this branch's claimed issues back to the repo backlog before the
    // branch row goes away — issues are repo-owned and must outlive teardown.
    weaver_core::issue::unclaim_branch(&st.db, &branch.repo_root, &branch.branch)
        .await
        .ok();
    // Drop the branch row too — deleting a session takes its branch with it.
    branch_mod::delete(&st.db, &branch.id).await?;
    if !warnings.is_empty() {
        tracing::warn!(branch = %branch.id, warnings = warnings.len(), "session removed with warnings");
    }
    Ok(Json(json!({ "deleted": true, "warnings": warnings })))
}

// ---------------------------------------------------------------------------
// Session actions
// ---------------------------------------------------------------------------

/// Archive a session: tear down its tmux and remove the worktree, but keep the
/// branch (and its commits), the session row, and run history.
/// This is the "I'm done with this workstream" action — unlike delete, the
/// weaver/loom record is preserved for future reference, and the git branch is
/// left intact so the work can be revisited or a worktree recreated later.
///
/// Extracted from the route handler so the GitHub poller can archive a session
/// the moment its PR merges (see [`crate::github::refresh`]). Returns any
/// non-fatal teardown warnings.
pub async fn archive(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<Vec<String>, AppError> {
    let mut warnings: Vec<String> = Vec::new();

    tmux::kill_session(&session.tmux_session).await.ok();
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    if work_dir.exists() {
        if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
            warnings.push(format!("worktree remove: {e}"));
            tokio::fs::remove_dir_all(&work_dir).await.ok();
        }
    }
    session_mod::set_status(&st.db, &session.id, "archived").await?;
    // An archived session is finished with: its agent is gone, so it can no
    // longer "need me". Clear any lingering attention so the dashboard stops
    // flagging a torn-down workstream. Attention is the agent's live "does this
    // need a human?" signal; nothing is live here any more. The history (goal,
    // status, events) is kept.
    if branch.attention != branch_mod::DEFAULT_ATTENTION {
        branch_mod::set_attention(&st.db, &branch.id, branch_mod::DEFAULT_ATTENTION).await?;
    }
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": "archived", "reason": "session archived" }),
    )
    .await
    .ok();
    if !warnings.is_empty() {
        tracing::warn!(branch = %branch.id, warnings = warnings.len(), "session archived with warnings");
    }
    Ok(warnings)
}

async fn archive_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let warnings = archive(&st, &session, &branch).await?;
    Ok(Json(
        json!({ "archived": true, "branch": branch.branch, "warnings": warnings }),
    ))
}

/// Refresh a session's GitHub PR snapshot on demand (the dashboard's "refresh"
/// affordance) and return the updated session. Manual refresh never
/// auto-archives — that surprise is reserved for the background poller, which
/// will pick a freshly-merged PR up within a tick.
async fn refresh_github_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    if !github::gh_available().await {
        return Err(AppError::bad_request(
            "the GitHub CLI (`gh`) is not available on the server",
        ));
    }
    github::refresh(&st, &session, &branch, false)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("gh: {e}")))?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(SessionView::build(&st.db, &session, &branch).await?))
}

/// Recreate an orphaned session's tmux and resume its agent.
pub async fn adopt(st: &AppState, session: &Session, branch: &Branch) -> Result<(), AppError> {
    if tmux::has_session(&session.tmux_session).await {
        return Err(AppError::conflict(
            "session already has a running tmux process",
        ));
    }
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(AppError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            session.work_dir
        )));
    }
    let goal_file = {
        let f = db::run_dir(&session.id).join("goal.txt");
        if f.exists() {
            Some(f)
        } else {
            None
        }
    };
    let base_args = config::get_or(&st.db, "agent.claude_args", "").await;
    let claude_args = agent::combine_args(&base_args, &session.model, &session.effort);
    agent::launch(
        &agent::LaunchSpec {
            branch_id: &branch.id,
            agent_kind: &session.agent_kind,
            work_dir: &work_dir,
            tmux_session: &session.tmux_session,
            goal_file: goal_file.as_deref(),
            server_addr: &st.addr,
            claude_args: &claude_args,
        },
        agent::LaunchMode::Adopt,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    session_mod::set_status(&st.db, &session.id, "launching").await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": "launching", "reason": "session adopted" }),
    )
    .await
    .ok();
    Ok(())
}

async fn adopt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    adopt(&st, &session, &branch).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(SessionView::build(&st.db, &session, &branch).await?))
}

// ---------------------------------------------------------------------------
// File viewer — a read-only window onto the worktree: a file tree, text content
// (for an embedded editor), and raw bytes (for images). The worktree is the
// agent's own checkout; these endpoints never write.
// ---------------------------------------------------------------------------

/// Text files larger than this aren't shipped to the editor — beyond a couple
/// of MB the browser editor stops being useful and starts being a memory hog.
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;

/// `base` selects the diff baseline: `branch` (default, the branch's fork point)
/// or `uncommitted` (vs `HEAD`). Shared by the tree and file endpoints; absent
/// or unknown values fall back to `branch` (see [`git::DiffBase::from_query`]).
#[derive(Debug, Deserialize)]
struct TreeQuery {
    base: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: String,
    /// `working` (default, read from disk) or `base` (read from the diff base).
    #[serde(rename = "ref")]
    reference: Option<String>,
    /// Which baseline the `base` ref resolves to — see [`TreeQuery`].
    base: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    path: String,
}

/// Validate a client-supplied repo-relative path: reject absolute paths and any
/// `.`/`..`/prefix component, so it cannot escape the worktree. Returns the
/// normalized (`/`-separated) relative path.
fn rel_path(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("path is required"));
    }
    let p = std::path::Path::new(trimmed);
    if p.is_absolute() {
        return Err(AppError::bad_request(
            "path must be relative to the worktree",
        ));
    }
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(AppError::bad_request(
            "path must not contain '.' or '..' segments",
        ));
    }
    Ok(trimmed.replace('\\', "/"))
}

/// Best-effort content type from the file extension, for the raw-bytes endpoint.
/// Only the formats the viewer renders inline get a real type; everything else
/// downloads as an opaque blob.
fn content_type_for(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The worktree file tree: every git-known path plus any base-only deletions, so
/// removed files are still browsable, with a `path → status` map of changes vs
/// the base branch. The flat list is assembled into a tree client-side.
async fn tree_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<TreeQuery>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let mode = git::DiffBase::from_query(q.base.as_deref());
    let files = git::list_files(&work_dir).await?;
    // A missing/odd base shouldn't sink the whole tree — just show no badges.
    let changed = match git::diff_since(&work_dir, &branch.base_branch, mode).await {
        Ok(since) => git::changed_files(&work_dir, &since)
            .await
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let changed: serde_json::Map<String, Value> = changed
        .into_iter()
        .map(|c| (c.path, Value::String(c.status)))
        .collect();
    Ok(Json(
        json!({ "files": files, "changed": changed, "base": mode.as_str() }),
    ))
}

/// Text content of a single worktree file for the editor. Binary and oversized
/// files report a flag instead of content so the client can fall back to the raw
/// endpoint or a placeholder. `ref=base` reads the file as of the merge-base
/// (the original side of a diff).
async fn file_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let rel = rel_path(&q.path)?;

    let bytes: Vec<u8> = if q.reference.as_deref() == Some("base") {
        let mode = git::DiffBase::from_query(q.base.as_deref());
        match git::diff_since(&work_dir, &branch.base_branch, mode).await {
            Ok(since) => git::read_blob(&work_dir, &since, &rel)
                .await?
                .unwrap_or_default(),
            // No base means nothing to compare against; treat as empty original.
            Err(_) => Vec::new(),
        }
    } else {
        match tokio::fs::read(work_dir.join(&rel)).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AppError::not_found("file"))
            }
            Err(e) => return Err(e.into()),
        }
    };

    let size = bytes.len();
    // A NUL byte in the first 8 KiB is the usual "this is binary" heuristic.
    let head = &bytes[..size.min(8192)];
    if head.contains(&0) {
        return Ok(Json(json!({ "path": rel, "binary": true, "bytes": size })));
    }
    if size > MAX_TEXT_BYTES {
        return Ok(Json(
            json!({ "path": rel, "too_large": true, "bytes": size }),
        ));
    }
    Ok(Json(json!({
        "path": rel,
        "content": String::from_utf8_lossy(&bytes),
        "bytes": size,
        "binary": false,
    })))
}

/// Raw bytes of a worktree file, with a guessed content type — for `<img>` tags
/// and downloads. Always reads the working tree (never a git ref).
async fn raw_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<RawQuery>,
) -> ApiResult<Response> {
    let (session, _) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let rel = rel_path(&q.path)?;
    let bytes = match tokio::fs::read(work_dir.join(&rel)).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::not_found("file"))
        }
        Err(e) => return Err(e.into()),
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type_for(&rel)),
            (header::CONTENT_DISPOSITION, "inline"),
        ],
        bytes,
    )
        .into_response())
}

/// Write raw bytes to a worktree file — the editor's save primitive (the
/// read-only `file`/`raw` endpoints never write). Reuses `rel_path` for
/// traversal safety and creates parent directories as needed, so a new
/// `docs/plans/<slug>.md` can be saved into a dir that doesn't exist yet.
async fn write_file(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<RawQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let rel = rel_path(&q.path)?;
    let target = work_dir.join(&rel);
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&target, &body).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "file_written",
        json!({ "path": rel }),
    )
    .await
    .ok();
    Ok(Json(json!({ "path": rel, "bytes": body.len() })))
}

// ---------------------------------------------------------------------------
// Plan view — a structured project plan rendered with task status PROJECTED
// from the issue ledger, plus reconcile. The plan FILE owns structure; the
// `issues` table owns state. See docs/structured-projects.md.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PlanTaskView {
    id: String,
    title: String,
    exec: String,
    value: String,
    deps: Vec<String>,
    /// Linked issue (the materialization), if any — the projected state.
    issue_id: Option<i64>,
    issue_status: Option<String>,
    claimed_branch: Option<String>,
}

#[derive(Debug, Serialize)]
struct PlanView {
    slug: String,
    /// Worktree-relative path, for the file-write (Edit) endpoint.
    path: String,
    title: String,
    status: String,
    /// Raw markdown source — the dashboard renders and edits this.
    content: String,
    tasks: Vec<PlanTaskView>,
    /// Every plan slug in the repo, for a picker.
    available: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlanQuery {
    slug: Option<String>,
}

/// The plan directory, worktree-relative, honoring `.weaver/config.toml`
/// `[plan].dir` (default `docs/plans`).
fn plan_dir_rel(work_dir: &std::path::Path, repo_root: &str) -> String {
    repo_config::plan_dir(&[work_dir.to_path_buf(), PathBuf::from(repo_root)])
}

/// Plan slugs (markdown file stems) present under `dir`, sorted.
fn plan_slugs(dir: &std::path::Path) -> Vec<String> {
    let mut slugs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    slugs.push(stem.to_string());
                }
            }
        }
    }
    slugs.sort();
    slugs
}

/// The plan a session is working, derived from its claimed issue's `plan_task`.
async fn claimed_plan_slug(db: &Db, branch: &Branch) -> Option<String> {
    let issues = weaver_core::issue::list_for_branch(db, &branch.repo_root, &branch.branch, true)
        .await
        .ok()?;
    issues
        .iter()
        .find_map(|i| i.plan_task.as_deref())
        .and_then(|k| k.split('#').next())
        .map(str::to_string)
}

/// A session's plan, parsed, with each task's status joined from the ledger.
/// `?slug=` selects a specific plan; otherwise the session's claimed-issue plan,
/// then the first available, is used.
async fn get_plan(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<PlanQuery>,
) -> ApiResult<Json<PlanView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let dir_rel = plan_dir_rel(&work_dir, &branch.repo_root);
    let available = plan_slugs(&work_dir.join(&dir_rel));

    let slug = match q.slug {
        Some(s) if !s.trim().is_empty() => s,
        _ => match claimed_plan_slug(&st.db, &branch).await {
            Some(s) => s,
            None => match available.first() {
                Some(s) => s.clone(),
                None => return Err(AppError::not_found("plan")),
            },
        },
    };

    let rel = format!("{dir_rel}/{slug}.md");
    let content = match tokio::fs::read_to_string(work_dir.join(&rel)).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::not_found("plan"))
        }
        Err(e) => return Err(e.into()),
    };

    let parsed = plan::parse(&slug, &content);
    let issues = weaver_core::issue::list_for_plan(&st.db, &branch.repo_root, &slug, true).await?;
    let tasks = parsed
        .tasks
        .iter()
        .map(|t| {
            let task_key = t.key(&slug);
            let issue = issues
                .iter()
                .find(|i| i.plan_task.as_deref() == Some(task_key.as_str()));
            PlanTaskView {
                id: t.id.clone(),
                title: t.title.clone(),
                exec: t.exec.clone(),
                value: t.value.clone(),
                deps: t.deps.clone(),
                issue_id: issue.map(|i| i.id),
                issue_status: issue.map(|i| i.status.clone()),
                claimed_branch: issue.and_then(|i| i.claimed_branch.clone()),
            }
        })
        .collect();

    Ok(Json(PlanView {
        slug,
        path: rel,
        title: parsed.title,
        status: parsed.status,
        content,
        tasks,
        available,
    }))
}

#[derive(Debug, Deserialize)]
struct SyncReq {
    slug: String,
    /// Apply the delta, not just preview it.
    #[serde(default)]
    apply: bool,
}

/// Reconcile a plan against the issue ledger. Returns the delta (and applies it
/// when `apply`). In-flight tasks are flagged, never rewritten.
async fn sync_plan(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SyncReq>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let dir_rel = plan_dir_rel(&work_dir, &branch.repo_root);
    let rel = format!("{dir_rel}/{}.md", req.slug);
    let content = match tokio::fs::read_to_string(work_dir.join(&rel)).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::not_found("plan"))
        }
        Err(e) => return Err(e.into()),
    };
    let parsed = plan::parse(&req.slug, &content);
    let issues =
        weaver_core::issue::list_for_plan(&st.db, &branch.repo_root, &req.slug, true).await?;
    let delta = plan::diff(&req.slug, &parsed.tasks, &issues);

    if req.apply && !delta.is_empty() {
        plan::apply(&st.db, &branch, &req.slug, &delta).await?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "plan_synced",
            json!({ "slug": req.slug, "actions": delta.actions.len() }),
        )
        .await
        .ok();
    }

    Ok(Json(json!({
        "applied": req.apply,
        "flags": delta.flags(),
        "actions": delta.actions,
    })))
}

// ---------------------------------------------------------------------------
// Scratch files — drag-and-drop reference material dropped into the worktree's
// `scratch/` directory so the agent can read it (e.g. "see scratch/error.log").
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ScratchQuery {
    name: String,
}

/// Validate a client-supplied scratch file name: a single path component, no
/// separators, no `.`/`..`. Returns the bare name on success.
fn scratch_name(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("file name is required"));
    }
    let name = std::path::Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name != trimmed || name == "." || name == ".." {
        return Err(AppError::bad_request(
            "file name must be a single path component",
        ));
    }
    Ok(name.to_string())
}

/// Write launch-time scratch files into `<work_dir>/scratch/`, returning the
/// bare names written (sorted, de-duplicated). The directory is git-ignored
/// exactly as [`upload_scratch`] does it, so reference material never enters
/// the agent's diff. The whole batch is rejected if any name or body is
/// malformed — a launch shouldn't half-succeed.
async fn write_initial_scratch(
    work_dir: &std::path::Path,
    files: &[ScratchUpload],
) -> ApiResult<Vec<String>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let dir = work_dir.join("scratch");
    tokio::fs::create_dir_all(&dir).await?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        tokio::fs::write(&gitignore, "*\n").await?;
    }
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    for f in files {
        let name = scratch_name(&f.name)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(f.content_base64.trim())
            .map_err(|e| {
                AppError::bad_request(format!("scratch file '{name}': invalid base64: {e}"))
            })?;
        tokio::fs::write(dir.join(&name), &bytes).await?;
        names.push(name);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// A sentence telling the agent about its launch-time scratch files, or `None`
/// when none were attached. Appended to the launch prompt so a fresh agent
/// knows the reference material exists without the user having to mention it.
fn scratch_note(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let list = names
        .iter()
        .map(|n| format!("scratch/{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Reference files have been attached for this task in the `scratch/` \
         directory of your worktree (it is kept out of git): {list}. \
         Read them as needed."
    ))
}

async fn list_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let dir = PathBuf::from(&session.work_dir).join("scratch");
    let mut out: Vec<Value> = Vec::new();
    match tokio::fs::read_dir(&dir).await {
        Ok(mut rd) => {
            while let Some(entry) = rd.next_entry().await? {
                let meta = entry.metadata().await?;
                if !meta.is_file() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    // Hide housekeeping dotfiles (e.g. the .gitignore we write).
                    if name.starts_with('.') {
                        continue;
                    }
                    out.push(json!({ "name": name, "bytes": meta.len() }));
                }
            }
        }
        // No scratch directory yet just means nothing has been dropped.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    out.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    Ok(Json(out))
}

async fn upload_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ScratchQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let name = scratch_name(&q.name)?;
    let dir = PathBuf::from(&session.work_dir).join("scratch");
    tokio::fs::create_dir_all(&dir).await?;
    // Reference material isn't meant to be committed; keep the whole directory
    // out of git so it never shows up in the agent's diff.
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        tokio::fs::write(&gitignore, "*\n").await?;
    }
    tokio::fs::write(dir.join(&name), &body).await?;
    Ok(Json(json!({
        "name": name,
        "bytes": body.len(),
        "path": format!("scratch/{name}"),
    })))
}

async fn delete_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ScratchQuery>,
) -> ApiResult<StatusCode> {
    let (session, _) = require_session(&st.db, &key).await?;
    let name = scratch_name(&q.name)?;
    let path = PathBuf::from(&session.work_dir).join("scratch").join(&name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found("scratch file"))
        }
        Err(e) => Err(e.into()),
    }
}

async fn log_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Event>>> {
    let branch = require_branch(&st.db, &key).await?;
    Ok(Json(events::history(&st.db, &branch.id, 200).await?))
}

async fn events_sse(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let branch = require_branch(&st.db, &key).await?;
    let id = branch.id;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |result| {
        let event = result.ok()?;
        if event.branch_id != id {
            return None;
        }
        Some(Ok(sse::Event::default()
            .event(event.kind.clone())
            .json_data(&event)
            .unwrap_or_default()))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// Driving a session's tmux pane (send / interrupt / preview)
//
// One-shot HTTP primitives for an agent (or script) to drive a child session
// uniformly, distinct from the interactive terminal WebSocket: type a message,
// interrupt the current turn, or read back the pane.
// ---------------------------------------------------------------------------

/// Guard the pane-driving endpoints: the session must have a live tmux to type
/// into or capture. An orphaned/torn-down session returns 409.
async fn require_live_tmux(session: &Session) -> ApiResult<()> {
    if tmux::has_session(&session.tmux_session).await {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "session '{}' has no live tmux to drive",
            session.id
        )))
    }
}

#[derive(Debug, Deserialize)]
struct SendReq {
    /// The text to type into the agent's pane.
    text: String,
    /// Whether to follow the text with Enter to submit it (and so trigger an
    /// agent round). Defaults to true; pass false to stage input unsubmitted.
    #[serde(default = "default_submit")]
    submit: bool,
}

fn default_submit() -> bool {
    true
}

/// Type a message into a session's agent pane and, by default, submit it with
/// Enter to trigger an agent round.
async fn send_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_live_tmux(&session).await?;
    tmux::send_literal(&session.tmux_session, &req.text)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if req.submit {
        tmux::send_enter(&session.tmux_session)
            .await
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(Json(json!({ "sent": true, "submitted": req.submit })))
}

/// Send a break/interrupt — `Escape`, the keystroke Claude Code reads as "stop
/// the current turn" — to a session's agent pane.
async fn interrupt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_live_tmux(&session).await?;
    tmux::send_key(&session.tmux_session, "Escape")
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "interrupted": true })))
}

#[derive(Debug, Deserialize)]
struct PreviewQuery {
    /// Extra scrollback lines to include above the visible screen (0 = just the
    /// visible pane).
    #[serde(default)]
    lines: usize,
}

/// Capture the session's tmux pane as plain text — "what does the child look
/// like right now". Returns `{ "screen": "<text>" }`.
async fn preview_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<PreviewQuery>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_live_tmux(&session).await?;
    let screen = tmux::capture(&session.tmux_session, q.lines)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "screen": screen })))
}

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

async fn list_branches(State(st): State<AppState>) -> ApiResult<Json<Vec<BranchView>>> {
    let branches = branch_mod::list(&st.db).await?;
    let mut out: Vec<BranchView> = Vec::with_capacity(branches.len());
    for b in branches {
        out.push(BranchView::build(&st.db, &b).await?);
    }
    Ok(Json(out))
}

async fn get_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    Ok(Json(BranchView::build(&st.db, &branch).await?))
}

#[derive(Debug, Deserialize)]
struct PatchBranchReq {
    title: Option<String>,
    goal: Option<String>,
    description: Option<String>,
    attention: Option<String>,
}

async fn patch_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchBranchReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    apply_attention_patch(&st, &branch, req.attention.as_deref()).await?;
    if let Some(title) = &req.title {
        branch_mod::set_title(&st.db, &branch.id, title).await?;
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal).await?;
    }
    if let Some(description) = &req.description {
        branch_mod::set_description(&st.db, &branch.id, description).await?;
    }
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    Ok(Json(BranchView::build(&st.db, &branch).await?))
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct IssueListQuery {
    #[serde(default)]
    all: bool,
}

/// Issues claimed by this branch — the session's working set.
async fn list_branch_issues(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<IssueListQuery>,
) -> ApiResult<Json<Vec<IssueView>>> {
    let branch = require_branch(&st.db, &key).await?;
    let issues =
        weaver_core::issue::list_for_branch(&st.db, &branch.repo_root, &branch.branch, q.all)
            .await?;
    Ok(Json(issues.into_iter().map(IssueView::from).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateIssueReq {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    github_issue: Option<i64>,
}

/// Create an issue claimed by this branch.
async fn create_branch_issue(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<CreateIssueReq>,
) -> ApiResult<Json<IssueView>> {
    if req.title.trim().is_empty() {
        return Err(AppError::bad_request("issue title is required"));
    }
    let branch = require_branch(&st.db, &key).await?;
    let issue = weaver_core::issue::add(
        &st.db,
        &weaver_core::issue::NewIssue {
            repo_root: branch.repo_root.clone(),
            source_branch: Some(branch.branch.clone()),
            claimed_branch: Some(branch.branch.clone()),
            title: req.title.trim().to_string(),
            body: req.body,
            github_issue: req.github_issue,
            ..Default::default()
        },
    )
    .await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "issue_added",
        json!({ "id": issue.id, "title": issue.title }),
    )
    .await
    .ok();
    Ok(Json(IssueView::from(issue)))
}

/// Resolve the branch row an issue event should be attributed to: the branch
/// currently working it, else the branch it came from. `None` for a pure
/// repo-level backlog item (no session feed to notify).
async fn issue_event_branch(db: &Db, issue: &Issue) -> Option<String> {
    let name = issue
        .claimed_branch
        .as_deref()
        .or(issue.source_branch.as_deref())?;
    let branch = branch_mod::find_by_repo_branch(db, &issue.repo_root, name)
        .await
        .ok()
        .flatten()?;
    Some(branch.id)
}

async fn get_issue(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<IssueView>> {
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    Ok(Json(IssueView::from(issue)))
}

#[derive(Debug, Deserialize)]
struct PatchIssueReq {
    title: Option<String>,
    body: Option<String>,
    /// "open" or "closed".
    status: Option<String>,
}

async fn patch_issue(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PatchIssueReq>,
) -> ApiResult<Json<IssueView>> {
    let existing = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    if let Some(status) = req.status.as_deref() {
        match status {
            "open" => weaver_core::issue::reopen(&st.db, id).await?,
            "closed" => weaver_core::issue::close(&st.db, id).await?,
            other => {
                return Err(AppError::bad_request(format!(
                    "invalid status '{other}' (expected 'open' or 'closed')"
                )));
            }
        }
        let kind = if status == "open" {
            "issue_reopened"
        } else {
            "issue_closed"
        };
        if let Some(branch_id) = issue_event_branch(&st.db, &existing).await {
            events::record(&st.db, &st.bus, &branch_id, kind, json!({ "id": id }))
                .await
                .ok();
        }
    }
    if req.title.is_some() || req.body.is_some() {
        let new_title = req.title.as_deref().unwrap_or(&existing.title);
        let new_body = req.body.as_deref().unwrap_or(&existing.body);
        sqlx::query("UPDATE issues SET title = ?, body = ?, updated_at = ? WHERE id = ?")
            .bind(new_title)
            .bind(new_body)
            .bind(weaver_core::db::now_iso())
            .bind(id)
            .execute(&st.db)
            .await?;
    }
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    Ok(Json(IssueView::from(issue)))
}

async fn delete_issue(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let _ = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    weaver_core::issue::delete(&st.db, id).await?;
    Ok(Json(json!({ "deleted": true })))
}

// ---------------------------------------------------------------------------
// Repo-scoped issues (the backlog / board surface)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RepoIssuesQuery {
    /// Repo to scope to (canonical primary-worktree path). The frontend has
    /// this from any `BranchView`.
    repo_root: Option<String>,
    /// Alternative for callers that only know a directory (e.g. the `loom`
    /// CLI): the repo root is resolved from it server-side.
    cwd: Option<String>,
    #[serde(default)]
    all: bool,
    /// `repo` (default) = every issue; `backlog` = unclaimed only.
    #[serde(default)]
    scope: Option<String>,
}

/// Resolve a repo identity from an explicit `repo_root` or, failing that, a
/// `cwd` — canonicalized to match how issues are keyed.
async fn resolve_repo_root(repo_root: Option<&str>, cwd: Option<&str>) -> ApiResult<String> {
    if let Some(rr) = repo_root.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(rr.to_string());
    }
    let cwd = cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("repo_root or cwd is required"))?;
    let root = git::repo_root(&PathBuf::from(cwd))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(root.canonicalize().unwrap_or(root).display().to_string())
}

/// The repo-wide issue board: every issue in a repo, or just the unclaimed
/// backlog with `?scope=backlog`.
async fn list_repo_issues(
    State(st): State<AppState>,
    Query(q): Query<RepoIssuesQuery>,
) -> ApiResult<Json<Vec<IssueView>>> {
    let repo_root = resolve_repo_root(q.repo_root.as_deref(), q.cwd.as_deref()).await?;
    let issues = match q.scope.as_deref() {
        Some("backlog") => weaver_core::issue::list_backlog(&st.db, &repo_root, q.all).await?,
        Some("repo") | None => weaver_core::issue::list_for_repo(&st.db, &repo_root, q.all).await?,
        Some(other) => {
            return Err(AppError::bad_request(format!(
                "invalid scope '{other}' (expected 'repo' or 'backlog')"
            )))
        }
    };
    Ok(Json(issues.into_iter().map(IssueView::from).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateRepoIssueReq {
    repo_root: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    github_issue: Option<i64>,
}

/// Create an unclaimed repo-level backlog item.
async fn create_repo_issue(
    State(st): State<AppState>,
    Json(req): Json<CreateRepoIssueReq>,
) -> ApiResult<Json<IssueView>> {
    if req.title.trim().is_empty() {
        return Err(AppError::bad_request("issue title is required"));
    }
    if req.repo_root.trim().is_empty() {
        return Err(AppError::bad_request("repo_root is required"));
    }
    let issue = weaver_core::issue::add(
        &st.db,
        &weaver_core::issue::NewIssue {
            repo_root: req.repo_root,
            title: req.title.trim().to_string(),
            body: req.body,
            github_issue: req.github_issue,
            ..Default::default()
        },
    )
    .await?;
    Ok(Json(IssueView::from(issue)))
}

// ---------------------------------------------------------------------------
// Recent repositories
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RecentReposQuery {
    limit: Option<i64>,
}

async fn recent_repos(
    State(st): State<AppState>,
    Query(q): Query<RecentReposQuery>,
) -> ApiResult<Json<Vec<repo::RecentRepo>>> {
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    Ok(Json(repo::recent(&st.db, limit).await?))
}

#[derive(Debug, Deserialize)]
struct BranchesQuery {
    cwd: String,
}

#[derive(Debug, Serialize)]
struct BranchInfo {
    name: String,
    worktree: Option<String>,
    current: bool,
}

async fn repo_branches(Query(q): Query<BranchesQuery>) -> ApiResult<Json<Vec<BranchInfo>>> {
    let cwd = PathBuf::from(&q.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let current = git::current_branch(&repo_root).await.ok();
    let names = git::list_branches(&repo_root).await?;
    let mut out: Vec<BranchInfo> = Vec::with_capacity(names.len());
    for name in names {
        let worktree = git::worktree_for_branch(&repo_root, &name)
            .await
            .ok()
            .flatten()
            .map(|p| p.display().to_string());
        let is_current = current.as_deref() == Some(name.as_str());
        out.push(BranchInfo {
            name,
            worktree,
            current: is_current,
        });
    }
    out.sort_by(|a, b| {
        let rank = |b: &BranchInfo| {
            if b.current {
                0
            } else if b.worktree.is_some() {
                1
            } else {
                2
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn settings_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "settings": config::describe(db).await? })))
}

async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    settings_envelope(&st.db).await
}

async fn patch_settings(
    State(st): State<AppState>,
    Json(body): Json<serde_json::Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let mut changes: Vec<config::Change> = Vec::with_capacity(body.len());
    let mut errors = serde_json::Map::new();

    for (key, raw) in body {
        if config::spec(&key).is_none() {
            errors.insert(key, json!("unknown setting"));
            continue;
        }
        let value = match raw {
            Value::Null => None,
            Value::String(s) => Some(s),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => {
                errors.insert(
                    key,
                    json!("value must be a string, number, boolean, or null"),
                );
                continue;
            }
        };
        if let Some(value) = &value {
            if let Err(why) = config::validate(&key, value) {
                errors.insert(key, json!(why));
                continue;
            }
        }
        changes.push((key, value));
    }

    if !errors.is_empty() {
        let message = if errors.len() == 1 {
            let (key, why) = errors.iter().next().unwrap();
            format!("{key}: {}", why.as_str().unwrap_or("invalid"))
        } else {
            "one or more settings are invalid".to_string()
        };
        return Err(AppError::bad_request(message).with_details(Value::Object(errors)));
    }
    config::apply(&st.db, &changes).await?;
    settings_envelope(&st.db).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[tokio::test]
    async fn create_tracking_issue_sources_parent_and_reuses_claims() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = AppState {
            db: db.clone(),
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
        };
        let child = branch_mod::upsert(&db, "/r", "weaver/child", "main")
            .await
            .unwrap();

        // A delegated launch names the parent as the issue's source.
        let id = create_tracking_issue(
            &st,
            &child,
            Some("weaver/parent"),
            "do it",
            "do it in detail",
            "",
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .expect("a fresh tracking issue");
        let issue = weaver_core::issue::get(&db, id).await.unwrap().unwrap();
        assert_eq!(issue.claimed_branch.as_deref(), Some("weaver/child"));
        assert_eq!(
            issue.source_branch.as_deref(),
            Some("weaver/parent"),
            "a delegated launch is sourced from the parent"
        );

        // A non-delegated launch is self-sourced (matches a hand-authored issue).
        let id2 =
            create_tracking_issue(&st, &child, None, "solo", "solo task", "", None, None, None)
                .await
                .unwrap()
                .unwrap();
        let issue2 = weaver_core::issue::get(&db, id2).await.unwrap().unwrap();
        assert_eq!(issue2.source_branch.as_deref(), Some("weaver/child"));

        // No task at all → nothing to track.
        let none = create_tracking_issue(&st, &child, None, "", "", "", None, None, None)
            .await
            .unwrap();
        assert!(none.is_none(), "an empty task opens no tracking issue");

        // Claiming an existing issue reuses it rather than opening a duplicate.
        let existing = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "preexisting".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let claimed = create_tracking_issue(
            &st,
            &child,
            None,
            "x",
            "x",
            "",
            None,
            None,
            Some(existing.id),
        )
        .await
        .unwrap();
        assert_eq!(claimed, Some(existing.id), "a claim reuses the issue id");
        let reclaimed = weaver_core::issue::get(&db, existing.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reclaimed.claimed_branch.as_deref(),
            Some("weaver/child"),
            "claiming stamps the new branch"
        );
    }

    #[test]
    fn tracking_note_names_the_issue_and_how_to_close_it() {
        let note = tracking_note(42);
        assert!(note.contains("weaver issue #42"));
        // It tells the agent exactly how to signal "done".
        assert!(note.contains("weaver issue close 42"));
        assert!(note.contains("weaver set-status"));
    }

    #[test]
    fn scratch_note_lists_files_or_is_empty() {
        assert!(scratch_note(&[]).is_none());
        let note = scratch_note(&["error.log".into(), "design.png".into()]).unwrap();
        assert!(note.contains("scratch/error.log"));
        assert!(note.contains("scratch/design.png"));
        // Mentions the directory so the agent knows where to look.
        assert!(note.contains("scratch/"));
    }

    #[tokio::test]
    async fn write_initial_scratch_drops_files_and_gitignores() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![
            ScratchUpload {
                name: "notes.txt".into(),
                content_base64: b64("hello scratch"),
            },
            ScratchUpload {
                name: "trace.log".into(),
                content_base64: b64("panic"),
            },
        ];
        let names = write_initial_scratch(dir.path(), &files).await.unwrap();
        assert_eq!(
            names,
            vec!["notes.txt".to_string(), "trace.log".to_string()]
        );

        let scratch = dir.path().join("scratch");
        assert_eq!(
            std::fs::read_to_string(scratch.join("notes.txt")).unwrap(),
            "hello scratch"
        );
        // The directory is kept out of git so reference material never enters
        // the agent's diff.
        assert_eq!(
            std::fs::read_to_string(scratch.join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[tokio::test]
    async fn write_initial_scratch_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        // A path-traversal name is refused (same rule as the upload endpoint).
        let bad_name = vec![ScratchUpload {
            name: "../escape".into(),
            content_base64: b64("x"),
        }];
        assert!(write_initial_scratch(dir.path(), &bad_name).await.is_err());
        // Malformed base64 is refused — a launch shouldn't half-write garbage.
        let bad_b64 = vec![ScratchUpload {
            name: "ok.txt".into(),
            content_base64: "not!base64!".into(),
        }];
        assert!(write_initial_scratch(dir.path(), &bad_b64).await.is_err());
        // Nothing to do for an empty batch.
        assert!(write_initial_scratch(dir.path(), &[])
            .await
            .unwrap()
            .is_empty());
    }
}
