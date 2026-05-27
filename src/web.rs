//! axum REST API + SSE. The CLI and the Vue SPA are both clients of this.

use std::convert::Infallible;
use std::path::PathBuf;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{self, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::db::Db;
use crate::events::{Event, EventBus};
use crate::workspace::{NewWorkspace, Workspace};
use crate::{agent, config, db, events, git, github, repo, tmux, workspace};

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

pub struct AppError {
    status: StatusCode,
    message: String,
    /// Optional machine-readable detail, e.g. a `{ key: reason }` map of
    /// per-field validation failures. Serialized under `"details"`.
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
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "workspace not found")
    }
    /// Attach a machine-readable detail payload (see [`AppError::details`]).
    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    /// The human-readable error message (for logging by non-HTTP callers).
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

async fn require(db: &Db, key: &str) -> ApiResult<Workspace> {
    workspace::resolve(db, key)
        .await?
        .ok_or_else(AppError::not_found)
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
        .route("/workspaces", get(list_workspaces).post(create_workspace))
        .route(
            "/workspaces/{id}",
            get(get_workspace)
                .patch(patch_workspace)
                .delete(delete_workspace),
        )
        .route("/workspaces/{id}/send", post(send_workspace))
        .route("/workspaces/{id}/interrupt", post(interrupt_workspace))
        .route("/workspaces/{id}/note", post(note_workspace))
        .route("/workspaces/{id}/summarize", post(summarize_workspace))
        .route("/workspaces/{id}/merge", post(merge_workspace))
        .route("/workspaces/{id}/adopt", post(adopt_workspace))
        .route("/workspaces/{id}/diff", get(diff_workspace))
        .route("/workspaces/{id}/pane", get(pane_workspace))
        .route("/workspaces/{id}/log", get(log_workspace))
        .route("/workspaces/{id}/events", get(events_sse))
        .route("/repos/recent", get(recent_repos))
        .route("/repos/branches", get(repo_branches))
        .route("/hook", post(hook))
        .route("/settings", get(get_settings).patch(patch_settings))
        .with_state(state);

    let index = static_dir().join("index.html");
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(static_dir()).fallback(ServeFile::new(index)))
        .layer(CorsLayer::permissive())
}

// ---------------------------------------------------------------------------
// Workspace CRUD
// ---------------------------------------------------------------------------

async fn list_workspaces(State(st): State<AppState>) -> ApiResult<Json<Vec<Workspace>>> {
    Ok(Json(workspace::list(&st.db).await?))
}

async fn get_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Workspace>> {
    Ok(Json(require(&st.db, &key).await?))
}

#[derive(Debug, Deserialize)]
struct CreateReq {
    /// Directory the user invoked `weaver new` from; resolved to a repo root.
    cwd: String,
    /// Human-readable title; derived from the goal when omitted.
    #[serde(default)]
    title: Option<String>,
    /// What the agent should do. Optional — an empty goal starts the agent
    /// with no initial prompt.
    #[serde(default)]
    goal: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    issue: Option<i64>,
    /// Attach to a branch that already exists locally instead of creating
    /// `weaver/<slug>`. If a worktree already checks out the branch, weaver
    /// reuses that path; otherwise it adds one under `.worktrees/<slug>`.
    /// Mutually exclusive with `name`.
    #[serde(default)]
    existing_branch: Option<String>,
}

async fn create_workspace(
    State(st): State<AppState>,
    Json(req): Json<CreateReq>,
) -> ApiResult<Json<Workspace>> {
    let cwd = PathBuf::from(&req.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    let id = workspace::new_id();
    let agent = match req.agent {
        Some(a) => a,
        None => config::get_or(&st.db, "agent.default", config::DEFAULT_AGENT).await,
    };

    // A workspace has a title, an (optional) goal, and a description. An
    // optional GitHub issue seeds all three.
    let mut goal = req.goal.unwrap_or_default().trim().to_string();
    let mut title = req
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let mut description = String::new();
    let mut github_repo = None;
    let mut github_issue = None;
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
    // A workspace always has a title; fall back to the goal, then a default.
    let title = title.unwrap_or_else(|| {
        if goal.is_empty() {
            "Untitled workspace".to_string()
        } else {
            workspace::derive_title(&goal)
        }
    });

    let existing = req
        .existing_branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    if existing.is_some() && req.name.as_deref().map(str::trim).is_some_and(|n| !n.is_empty()) {
        return Err(AppError::bad_request(
            "`name` and `existing_branch` are mutually exclusive",
        ));
    }

    let base = match req.base.clone() {
        Some(b) => b,
        None => git::current_branch(&repo_root).await?,
    };

    let (slug, branch, work_dir) = if let Some(existing_branch) = existing {
        // Attach to an existing branch: reuse its worktree if one is checked
        // out, otherwise add one under `.worktrees/<slug>`. The branch name
        // is preserved verbatim — no `weaver/` prefix.
        if !git::branch_exists(&repo_root, existing_branch).await {
            return Err(AppError::bad_request(format!(
                "branch '{existing_branch}' does not exist in this repo"
            )));
        }
        let base_slug = workspace::slugify(existing_branch);
        let mut slug = base_slug.clone();
        let mut suffix = 2;
        while workspace::find_by_name(&st.db, &slug).await?.is_some() {
            slug = format!("{base_slug}-{suffix}");
            suffix += 1;
        }
        let work_dir = match git::worktree_for_branch(&repo_root, existing_branch)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?
        {
            Some(p) => p,
            None => {
                let dir = repo_root.join(".worktrees").join(&slug);
                tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
                git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
                git::worktree_add_existing(&repo_root, &dir, existing_branch)
                    .await
                    .map_err(|e| AppError::bad_request(e.to_string()))?;
                dir
            }
        };
        (slug, existing_branch.to_string(), work_dir)
    } else {
        // The slug: an explicit name wins, otherwise it is derived from the title.
        // It is always slugified so it is safe as both a branch and directory name.
        let explicit = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let base_slug = workspace::slugify(explicit.unwrap_or(title.as_str()));
        let mut slug = base_slug.clone();
        let mut suffix = 2;
        loop {
            let branch = format!("weaver/{slug}");
            let dir = repo_root.join(".worktrees").join(&slug);
            if !git::branch_exists(&repo_root, &branch).await && !dir.exists() {
                break;
            }
            if explicit.is_some() {
                return Err(AppError::conflict(format!(
                    "a workspace named '{slug}' already exists — choose a different name"
                )));
            }
            slug = format!("{base_slug}-{suffix}");
            suffix += 1;
        }
        let branch = format!("weaver/{slug}");
        let work_dir = repo_root.join(".worktrees").join(&slug);
        tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
        git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
        git::worktree_add(&repo_root, &work_dir, &branch, &base)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        (slug, branch, work_dir)
    };

    let run_dir = db::run_dir(&id);
    tokio::fs::create_dir_all(&run_dir).await?;
    // The goal file seeds the agent's first prompt; with no goal there is no
    // file and the agent launches unprompted.
    let goal_file = if goal.is_empty() {
        None
    } else {
        let f = run_dir.join("goal.txt");
        tokio::fs::write(&f, &goal).await?;
        Some(f)
    };

    // Launch the agent in a detached tmux session via the shared launch path
    // (installs Claude Code hooks, sets env, starts tmux).
    let session = format!("weaver-{id}");
    let claude_args = config::get_or(&st.db, "agent.claude_args", "").await;
    agent::launch(
        &agent::LaunchSpec {
            workspace_id: &id,
            agent_kind: &agent,
            work_dir: &work_dir,
            tmux_session: &session,
            goal_file: goal_file.as_deref(),
            server_addr: &st.addr,
            claude_args: &claude_args,
        },
        agent::LaunchMode::Fresh,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let status = if matches!(agent.as_str(), "shell" | "none") {
        "idle"
    } else {
        "launching"
    };
    let ws = workspace::insert(
        &st.db,
        &NewWorkspace {
            id: id.clone(),
            name: slug,
            title,
            goal,
            description,
            status: status.to_string(),
            repo_root: repo_root.display().to_string(),
            work_dir: work_dir.display().to_string(),
            branch,
            base_branch: base,
            tmux_session: session,
            agent_kind: agent,
            github_repo,
            github_issue,
        },
    )
    .await?;
    // Remember this repo so the dashboard can offer it for the next workspace.
    if let Err(e) = repo::record_use(&st.db, &ws.repo_root).await {
        tracing::warn!(workspace = %ws.id, error = %e, "failed to record recent repo");
    }
    events::record(
        &st.db,
        &st.bus,
        &id,
        "status",
        json!({ "status": status, "reason": "workspace created" }),
    )
    .await
    .ok();
    tracing::info!(workspace = %ws.id, name = %ws.name, status = %ws.status, agent = %ws.agent_kind, "workspace created");
    Ok(Json(ws))
}

#[derive(Debug, Deserialize)]
struct PatchReq {
    title: Option<String>,
    goal: Option<String>,
    description: Option<String>,
    status: Option<String>,
}

async fn patch_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchReq>,
) -> ApiResult<Json<Workspace>> {
    let ws = require(&st.db, &key).await?;
    if let Some(title) = &req.title {
        workspace::set_title(&st.db, &ws.id, title).await?;
    }
    if let Some(goal) = &req.goal {
        workspace::set_goal(&st.db, &ws.id, goal).await?;
        tokio::fs::write(db::run_dir(&ws.id).join("goal.txt"), goal)
            .await
            .ok();
    }
    if let Some(description) = &req.description {
        workspace::set_description(&st.db, &ws.id, description).await?;
        events::record(
            &st.db,
            &st.bus,
            &ws.id,
            "note",
            json!({ "text": "description updated" }),
        )
        .await
        .ok();
    }
    if let Some(status) = &req.status {
        if !workspace::STATUSES.contains(&status.as_str()) {
            return Err(AppError::bad_request(format!("invalid status '{status}'")));
        }
        workspace::set_status(&st.db, &ws.id, status).await?;
        events::record(
            &st.db,
            &st.bus,
            &ws.id,
            "status",
            json!({ "status": status, "source": "manual" }),
        )
        .await
        .ok();
    }
    Ok(Json(require(&st.db, &ws.id).await?))
}

#[derive(Debug, Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    keep_branch: bool,
}

async fn delete_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    let mut warnings: Vec<String> = Vec::new();

    tmux::kill_session(&ws.tmux_session).await.ok();
    let repo_root = PathBuf::from(&ws.repo_root);
    let work_dir = PathBuf::from(&ws.work_dir);
    if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
        warnings.push(format!("worktree remove: {e}"));
        tokio::fs::remove_dir_all(&work_dir).await.ok();
    }
    if !q.keep_branch {
        if let Err(e) = git::delete_branch(&repo_root, &ws.branch).await {
            warnings.push(format!("delete branch: {e}"));
        }
    }
    tokio::fs::remove_dir_all(db::run_dir(&ws.id)).await.ok();
    workspace::delete(&st.db, &ws.id).await?;
    if !warnings.is_empty() {
        tracing::warn!(workspace = %ws.id, warnings = warnings.len(), "workspace removed with warnings");
    }
    tracing::info!(workspace = %ws.id, name = %ws.name, keep_branch = q.keep_branch, "workspace removed");
    Ok(Json(json!({ "deleted": true, "warnings": warnings })))
}

// ---------------------------------------------------------------------------
// Workspace actions
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SendReq {
    text: String,
}

async fn send_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    tmux::send_text(&ws.tmux_session, &req.text)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    tracing::debug!(workspace = %ws.id, text_len = req.text.len(), "text sent to agent");
    workspace::touch(&st.db, &ws.id).await.ok();
    events::record(
        &st.db,
        &st.bus,
        &ws.id,
        "note",
        json!({ "text": format!("sent to agent: {}", req.text) }),
    )
    .await
    .ok();
    Ok(Json(json!({ "sent": true })))
}

/// Interrupt the agent by sending an Escape keypress to its tmux pane — the
/// same key a user would press in the TUI to stop the agent mid-task. It does
/// not kill the session; the agent stays alive and ready for the next prompt.
async fn interrupt_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    tmux::send_keys(&ws.tmux_session, &["Escape"])
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    tracing::debug!(workspace = %ws.id, "interrupt sent to agent");
    workspace::touch(&st.db, &ws.id).await.ok();
    events::record(
        &st.db,
        &st.bus,
        &ws.id,
        "note",
        json!({ "text": "interrupted agent (Esc)" }),
    )
    .await
    .ok();
    Ok(Json(json!({ "interrupted": true })))
}

#[derive(Debug, Deserialize)]
struct NoteReq {
    text: String,
}

async fn note_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<NoteReq>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    events::record(&st.db, &st.bus, &ws.id, "note", json!({ "text": req.text })).await?;
    workspace::touch(&st.db, &ws.id).await.ok();
    Ok(Json(json!({ "ok": true })))
}

async fn summarize_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    let description = crate::summary::summarize_workspace(&st, &ws)
        .await
        .map_err(|e| {
            tracing::error!(workspace = %ws.id, error = %e, "claude summary failed");
            AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    tracing::info!(workspace = %ws.id, description_len = description.len(), "workspace summarized");
    Ok(Json(json!({ "description": description })))
}

async fn merge_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    let repo_root = PathBuf::from(&ws.repo_root);
    if !git::is_clean(&repo_root).await? {
        return Err(AppError::conflict(
            "main checkout has uncommitted changes; commit or stash, then merge",
        ));
    }
    let current = git::current_branch(&repo_root).await?;
    if current != ws.base_branch {
        return Err(AppError::conflict(format!(
            "repo is on '{current}', expected base branch '{}'",
            ws.base_branch
        )));
    }
    let output = git::merge(&repo_root, &ws.branch)
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    workspace::set_status(&st.db, &ws.id, "done").await?;
    events::record(
        &st.db,
        &st.bus,
        &ws.id,
        "status",
        json!({ "status": "done", "reason": "merged" }),
    )
    .await
    .ok();
    tracing::info!(workspace = %ws.id, branch = %ws.branch, base = %ws.base_branch, "workspace merged");
    Ok(Json(json!({ "merged": true, "branch": ws.branch, "output": output })))
}

/// Recreate an orphaned workspace's tmux session and resume its agent.
///
/// Shared by the `POST /workspaces/{id}/adopt` handler and the server's
/// startup reconcile step. Fails if a session is already running for the
/// workspace or if its worktree no longer exists on disk; on success it
/// relaunches the agent in [`agent::LaunchMode::Adopt`], sets the status to
/// `launching`, and records a `status` event.
pub async fn adopt(st: &AppState, ws: &Workspace) -> Result<(), AppError> {
    if tmux::has_session(&ws.tmux_session).await {
        return Err(AppError::conflict(
            "workspace already has a running tmux session",
        ));
    }
    let work_dir = PathBuf::from(&ws.work_dir);
    if !work_dir.exists() {
        return Err(AppError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            ws.work_dir
        )));
    }
    let goal_file = {
        let f = db::run_dir(&ws.id).join("goal.txt");
        if f.exists() {
            Some(f)
        } else {
            None
        }
    };
    let claude_args = config::get_or(&st.db, "agent.claude_args", "").await;
    agent::launch(
        &agent::LaunchSpec {
            workspace_id: &ws.id,
            agent_kind: &ws.agent_kind,
            work_dir: &work_dir,
            tmux_session: &ws.tmux_session,
            goal_file: goal_file.as_deref(),
            server_addr: &st.addr,
            claude_args: &claude_args,
        },
        agent::LaunchMode::Adopt,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    workspace::set_status(&st.db, &ws.id, "launching").await?;
    events::record(
        &st.db,
        &st.bus,
        &ws.id,
        "status",
        json!({ "status": "launching", "reason": "workspace adopted" }),
    )
    .await
    .ok();
    tracing::info!(workspace = %ws.id, name = %ws.name, "workspace adopted");
    Ok(())
}

async fn adopt_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Workspace>> {
    let ws = require(&st.db, &key).await?;
    adopt(&st, &ws).await?;
    Ok(Json(require(&st.db, &ws.id).await?))
}

async fn diff_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    let work_dir = PathBuf::from(&ws.work_dir);
    let base = git::merge_base(&work_dir, &ws.base_branch).await?;
    let patch = git::diff(&work_dir, &base).await?;
    let stat = git::diff_stat(&work_dir, &base).await?;
    Ok(Json(json!({
        "base": ws.base_branch,
        "stat": stat,
        "patch": patch,
    })))
}

async fn pane_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &key).await?;
    let content = tmux::capture(&ws.tmux_session, 2000).await.unwrap_or_default();
    Ok(Json(json!({ "content": content })))
}

async fn log_workspace(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Event>>> {
    let ws = require(&st.db, &key).await?;
    Ok(Json(events::history(&st.db, &ws.id, 200).await?))
}

async fn events_sse(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let ws = require(&st.db, &key).await?;
    let id = ws.id;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |result| {
        let event = result.ok()?;
        if event.workspace_id != id {
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
// Recent repositories
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RecentReposQuery {
    /// How many repos to return. Defaults to 10; clamped to [1, 50].
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
    /// Directory used to resolve the repo root (same as `cwd` on create).
    cwd: String,
}

#[derive(Debug, Serialize)]
struct BranchInfo {
    name: String,
    /// Existing worktree path for this branch, if any is checked out.
    worktree: Option<String>,
    /// True for the repo's currently checked-out branch.
    current: bool,
}

async fn repo_branches(
    Query(q): Query<BranchesQuery>,
) -> ApiResult<Json<Vec<BranchInfo>>> {
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
    // Sort: current branch first, then branches with existing worktrees, then
    // the rest alphabetical.
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
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Hooks & settings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HookReq {
    workspace: String,
    event: String,
}

async fn hook(State(st): State<AppState>, Json(req): Json<HookReq>) -> ApiResult<Json<Value>> {
    let ws = require(&st.db, &req.workspace).await?;
    let status = match req.event.as_str() {
        "working" => "working",
        "waiting" => "waiting",
        "idle" => "idle",
        other => return Err(AppError::bad_request(format!("unknown hook event '{other}'"))),
    };
    workspace::set_status(&st.db, &ws.id, status).await?;
    workspace::touch(&st.db, &ws.id).await?;

    // On `waiting`, snapshot the tmux pane so the dashboard can show what the
    // agent is blocked on; clear it again as soon as the agent moves on.
    let prompt = if status == "waiting" {
        tmux::capture(&ws.tmux_session, 0)
            .await
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    workspace::set_pending_prompt(&st.db, &ws.id, &prompt).await?;

    let mut data = json!({ "status": status, "source": "hook" });
    if !prompt.is_empty() {
        data["prompt"] = json!(prompt);
    }
    events::record(&st.db, &st.bus, &ws.id, "status", data).await?;
    tracing::debug!(workspace = %ws.id, event = %req.event, status = %status, "hook handled");
    Ok(Json(json!({ "ok": true, "status": status })))
}

/// The canonical settings representation: every registered setting with its
/// label, help text, type, default, and current effective value, wrapped in an
/// envelope so the response can grow new fields without breaking clients.
async fn settings_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "settings": config::describe(db).await? })))
}

/// `GET /api/settings` — the full settings list. Both the web pane and the CLI
/// (`weaver config`) read this single shape.
async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    settings_envelope(&st.db).await
}

/// `PATCH /api/settings` — apply a partial map of changes.
///
/// The body is a JSON object of `{ "<key>": <value> }`: a string/number/bool
/// writes that key, `null` resets it to its default. Every key must be a
/// registered setting and every value must fit its type; the whole batch is
/// validated before anything is written, and applied atomically. On success
/// the response is the same envelope as [`get_settings`], reflecting the new
/// state — so a client never needs a follow-up request.
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
        // Coerce the JSON value to the stored string form; `null` means reset.
        let value = match raw {
            Value::Null => None,
            Value::String(s) => Some(s),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => {
                errors.insert(key, json!("value must be a string, number, boolean, or null"));
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
        // With a single bad key, surface its reason as the top-level message so
        // one-shot clients (the CLI) show something precise without having to
        // parse `details`.
        let message = if errors.len() == 1 {
            let (key, why) = errors.iter().next().unwrap();
            format!("{key}: {}", why.as_str().unwrap_or("invalid"))
        } else {
            "one or more settings are invalid".to_string()
        };
        return Err(AppError::bad_request(message).with_details(Value::Object(errors)));
    }
    config::apply(&st.db, &changes).await?;
    tracing::debug!(count = changes.len(), "settings updated");
    settings_envelope(&st.db).await
}
