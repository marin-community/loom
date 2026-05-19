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
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::db::Db;
use crate::events::{Event, EventBus};
use crate::workspace::{NewWorkspace, Workspace};
use crate::{agent, config, db, events, git, github, tmux, workspace};

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
}

impl AppError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
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
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
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
        .route("/workspaces/{id}/note", post(note_workspace))
        .route("/workspaces/{id}/summarize", post(summarize_workspace))
        .route("/workspaces/{id}/merge", post(merge_workspace))
        .route("/workspaces/{id}/diff", get(diff_workspace))
        .route("/workspaces/{id}/pane", get(pane_workspace))
        .route("/workspaces/{id}/log", get(log_workspace))
        .route("/workspaces/{id}/events", get(events_sse))
        .route("/hook", post(hook))
        .route("/settings", get(list_settings).post(set_setting))
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
    goal: String,
    /// Directory the user invoked `weaver new` from; resolved to a repo root.
    cwd: String,
    base: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    issue: Option<i64>,
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

    // Optional GitHub issue seeds the goal/description.
    let mut goal = req.goal.trim().to_string();
    let mut description = String::new();
    let mut github_repo = None;
    let mut github_issue = None;
    if let Some(number) = req.issue {
        let issue = github::fetch_issue(&repo_root, number)
            .await
            .map_err(|e| AppError::bad_request(format!("issue #{number}: {e}")))?;
        if goal.is_empty() {
            goal = issue.title.clone();
        }
        description = issue.body.clone();
        github_issue = Some(number);
        github_repo = github::repo_slug(&repo_root).await.ok();
    }
    if goal.is_empty() {
        return Err(AppError::bad_request("goal is required"));
    }

    let name = req
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| workspace::slugify(&goal));
    let base = match req.base {
        Some(b) => b,
        None => git::current_branch(&repo_root).await?,
    };
    let mut branch = format!("weaver/{name}");
    if git::branch_exists(&repo_root, &branch).await {
        branch = format!("weaver/{name}-{id}");
    }

    // Lay out filesystem: worktree + runtime dir + goal file.
    let work_dir = db::tree_dir(&id);
    let run_dir = db::run_dir(&id);
    if let Some(parent) = work_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::create_dir_all(&run_dir).await?;
    git::worktree_add(&repo_root, &work_dir, &branch, &base)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let goal_file = run_dir.join("goal.txt");
    tokio::fs::write(&goal_file, &goal).await?;

    // Install Claude Code status hooks for claude-backed workspaces.
    let weaver_bin = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "weaver".to_string());
    if agent == "claude" {
        agent::install_hooks(&work_dir, &weaver_bin, &id).await.ok();
    }

    // Launch the agent in a detached tmux session.
    let session = format!("weaver-{id}");
    let api_url = format!("http://{}", st.addr);
    let env = [
        ("WEAVER_API", api_url.as_str()),
        ("WEAVER_WORKSPACE", id.as_str()),
    ];
    let script = agent::launch_script(&agent, &goal_file, &env);
    tmux::new_session(&session, &work_dir, &script)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("tmux: {e}")))?;

    let status = if matches!(agent.as_str(), "shell" | "none") {
        "idle"
    } else {
        "launching"
    };
    let ws = workspace::insert(
        &st.db,
        &NewWorkspace {
            id: id.clone(),
            name,
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
    events::record(
        &st.db,
        &st.bus,
        &id,
        "status",
        json!({ "status": status, "reason": "workspace created" }),
    )
    .await
    .ok();
    Ok(Json(ws))
}

#[derive(Debug, Deserialize)]
struct PatchReq {
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
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    Ok(Json(json!({ "merged": true, "branch": ws.branch, "output": output })))
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
    events::record(
        &st.db,
        &st.bus,
        &ws.id,
        "status",
        json!({ "status": status, "source": "hook" }),
    )
    .await?;
    Ok(Json(json!({ "ok": true, "status": status })))
}

async fn list_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let map: serde_json::Map<String, Value> = config::list(&st.db)
        .await?
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    Ok(Json(Value::Object(map)))
}

#[derive(Debug, Deserialize)]
struct SettingReq {
    key: String,
    value: String,
}

async fn set_setting(
    State(st): State<AppState>,
    Json(req): Json<SettingReq>,
) -> ApiResult<Json<Value>> {
    config::set(&st.db, &req.key, &req.value).await?;
    Ok(Json(json!({ "ok": true })))
}
