//! axum REST API + SSE. The Vue SPA is the primary consumer.
//!
//! Endpoint layout (post phase-4 rename):
//!
//! * `/api/sessions` — list + create active sessions (each session is one
//!   terminal + one agent attached to a branch).
//! * `/api/sessions/{id}` — GET / PATCH / DELETE a single session, plus the
//!   action subroutes `/archive`, `/adopt`, `/tags/{key}` (PUT to set a tag,
//!   DELETE to clear it), `/log`, `/events`, and `/terminal` (a WebSocket
//!   bridged to the session's terminal via a PTY — see `crate::terminal`).
//!   Interacting with the agent (keystrokes, keys, TUIs) happens entirely over
//!   `/terminal`.
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
//!   "term_session": "weaver-abcd1234",
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
//!     "description": "...",         // current-state message (weaver status)
//!     "tags": [                     // every (key, value) annotation on the branch
//!       { "key": "attention", "value": "blocked", "note": "...",
//!         "set_by": "agent", "set_at": "..." }
//!     ],
//!     "repo_root": "/path/to/repo",
//!     "branch": "weaver/feature-x",
//!     "base_branch": "main",
//!     "created_at": "...",
//!     "updated_at": "...",
//!     "open_issue_count": 0
//!   }
//! }
//! ```
//!
//! A branch's status axes — the agent's self-reported `attention` and an
//! overlooker's `triage` — are **tags**: well-known keys under `tags`, set
//! through `PUT /api/sessions/{id}/tags/{key}` and cleared through `DELETE`.
//! Absence is the calm state; there is no stored `ok` tag.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, PathBuf};

use axum::{
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{
        sse::{self, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Extension, Json, Router,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::auth::{self, Principal};
use crate::db::Db;
use crate::events::{Event, EventBus};
use crate::session::{self as session_mod, NewSession, Session};
use crate::{
    agent, agent_env, backend, config, db, events, git, github, github_trigger,
    overlooker as ov_engine, repo,
};
use weaver_api::{
    AddUserReq, AgentOneshotReq, ArtifactMeta, ArtifactRefs, ArtifactView, ArtifactWriteBody,
    AuthMethods, BranchView, CreateIssueReq, CreateOverlookerReq, CreateRepoIssueReq, CreateReq,
    CreateTokenReq, CreatedTokenView, GithubConfigView, IssueRefStatus, IssueView, LoginReq,
    MeView, OverlookerRunView, OverlookerView, PatchIssueReq, PatchOverlookerReq, PatchSessionReq,
    ProgramView, RunOverlookerReq, ScratchUpload, SendReq, SessionView, SetGithubConfigReq,
    SetPasswordReq, TagReq, TokenView, UserView,
};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::issue::Issue;
use weaver_core::overlooker::{self as ov, Overlooker};
use weaver_core::tags;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub bus: EventBus,
    /// host:port the server is bound to, used to build child-process env.
    pub addr: String,
    /// Per-session embedded code-server lifecycle + reverse-proxy registry.
    pub ide: std::sync::Arc<crate::ide::IdeManager>,
    /// The inbound GitHub trigger: its GitHub gateway (the `gh`-backed default)
    /// and per-repo rate limiter. Shared across requests; a test swaps in a fake
    /// gateway via [`crate::github_trigger::GithubTrigger::with_gateway`].
    pub trigger: std::sync::Arc<crate::github_trigger::GithubTrigger>,
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
//
// The wire structs (`BranchView`, `SessionView`, `IssueView`, …) live in
// `weaver-api` — the one definition the server, the CLI, and the Python binding
// share. The async builders below gather the parts the daemon owns (open-issue
// counts, GitHub snapshots, run history) and hand them to the `from_parts`
// constructors. The DB access stays here; the wire shape stays there.
// ---------------------------------------------------------------------------

/// Build a [`BranchView`] for a branch, joining its tags, the denormalized
/// open-issue count, and the latest GitHub snapshot from the database.
async fn branch_view(db: &Db, branch: &Branch) -> ApiResult<BranchView> {
    // Every tag (the agent's `attention`, an overlooker's `triage`, any free-form
    // key) the dashboard resolves into a badge or a pill.
    let tags = tags::list(db, &branch.id).await?;
    // The badge counts the work this branch has claimed, not the whole repo.
    let open = weaver_core::issue::open_count_for_branch(db, &branch.repo_root, &branch.branch)
        .await
        .unwrap_or(0);
    // Best-effort: a missing/erroring snapshot just renders as no GitHub info.
    let github = github::get_status(db, &branch.id).await.ok().flatten();
    Ok(BranchView::from_parts(branch, &tags, open, github))
}

/// Build a [`SessionView`] for a session + its branch. `tracking_issue` is left
/// `None`; only the create path fills it.
async fn session_view(db: &Db, session: &Session, branch: &Branch) -> ApiResult<SessionView> {
    let bv = branch_view(db, branch).await?;
    Ok(SessionView {
        id: session.id.clone(),
        status: session.status.clone(),
        work_dir: session.work_dir.clone(),
        term_session: session.term_session.clone(),
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
        created_by: session.created_by.clone(),
        tracking_issue: None,
        branch: bv,
    })
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

async fn list_agents(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let default_agent = configured_agent(&st.db, "agent.default", config::DEFAULT_AGENT).await;
    Ok(Json(json!({
        "agents": agent::agent_metadata(),
        "default_agent": default_agent,
    })))
}

// ---------------------------------------------------------------------------
// Caching middleware
// ---------------------------------------------------------------------------

/// Whether `path` (the `/api`-stripped path) is an embedded-editor proxy route
/// — `/sessions/<id>/ide` or `/sessions/<id>/ide/…` — as opposed to the small
/// `ide-info` JSON probe, which is fine to ETag.
/// Paths under the embedded-editor reverse proxy (`…/sessions/{id}/ide`), which
/// must bypass the ETag middleware — buffering code-server's stream to hash it
/// truncates assets past the 16 MB cap. The middleware sees the nest-stripped
/// `/sessions/…` form, but we strip an optional leading `/api` too so the
/// exclusion survives if that layer is ever hoisted to the outer router.
fn is_ide_proxy_path(path: &str) -> bool {
    let path = path.strip_prefix("/api").unwrap_or(path);
    let Some(rest) = path.strip_prefix("/sessions/") else {
        return false;
    };
    match rest.split_once('/') {
        Some((_id, after)) => after == "ide" || after.starts_with("ide/"),
        None => false,
    }
}

/// Add `ETag` + `Cache-Control: no-cache` to JSON API GET responses and serve
/// `304 Not Modified` when the client's `If-None-Match` matches.
///
/// Skips non-200 responses, SSE streams, WebSocket upgrades, and the
/// embedded-editor proxy so they pass through untouched.
pub async fn api_etag_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    // The embedded-editor reverse proxy streams arbitrary code-server traffic
    // (assets, its own API, WebSockets). Buffering it to hash an ETag is both
    // wasteful and, past the 16 MB cap below, corrupting — so skip it entirely.
    if is_ide_proxy_path(request.uri().path()) {
        return next.run(request).await;
    }
    let if_none_match = request.headers().get(header::IF_NONE_MATCH).cloned();
    let response = next.run(request).await;

    if response.status() != StatusCode::OK {
        return response;
    }
    // Skip streaming responses (SSE, WebSocket upgrades).
    if let Some(ct) = response.headers().get(header::CONTENT_TYPE) {
        if ct.as_bytes().starts_with(b"text/event-stream") {
            return response;
        }
    }
    if response.headers().contains_key(header::UPGRADE) {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Response::from_parts(parts, axum::body::Body::empty()),
    };

    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let etag = format!("\"loom-{:016x}\"", hasher.finish());
    let etag_val: axum::http::HeaderValue = etag.parse().unwrap();

    parts.headers.insert(header::ETAG, etag_val.clone());
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert_with(|| "no-cache".parse().unwrap());

    if if_none_match.is_some_and(|v| v == etag_val) {
        parts.status = StatusCode::NOT_MODIFIED;
        return Response::from_parts(parts, axum::body::Body::empty());
    }

    Response::from_parts(parts, axum::body::Body::from(bytes))
}

/// Set `Cache-Control` on static asset responses:
/// - Content-hashed assets (filename contains an 8-hex-char segment, e.g.
///   `app.a1b2c3d4.js`) get `max-age=31536000, immutable` — the hash guarantees
///   the content never changes for that URL.
/// - Everything else (`index.html`, fonts, etc.) gets `no-cache` so browsers
///   always revalidate; `ServeDir` provides `ETag`/`Last-Modified` for fast 304s.
pub async fn static_cache_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;

    if response.status() != StatusCode::OK {
        return response;
    }

    let cache_control = if is_immutable_asset(&path) {
        "max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert_with(|| cache_control.parse().unwrap());
    Response::from_parts(parts, body)
}

/// True for content-hashed static assets produced by rspack.
/// Matches filenames like `app.a1b2c3d4.js` — any path component that is
/// exactly 8 lowercase hex characters surrounded by dots.
fn is_immutable_asset(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or("");
    filename
        .split('.')
        .any(|seg| seg.len() == 8 && seg.bytes().all(|b| b.is_ascii_hexdigit()))
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
    // Public surface: the liveness probe and the login flow itself. No
    // middleware — these must work for an unauthenticated caller, since they are
    // how one *becomes* authenticated.
    let public = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/auth/me", get(auth_me))
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .route("/auth/github/login", get(github_login))
        .route("/auth/github/callback", get(github_callback))
        // The inbound GitHub webhook. Deliberately OUTSIDE `require_auth`: it is
        // authenticated cryptographically by the HMAC signature it carries, not
        // by a loom principal. The handler is the untrusted-input boundary.
        .route("/github/webhook", post(github_webhook));

    // Everything else requires an authenticated principal — a bearer token, a
    // session cookie, or a trusted-loopback request — gated by `require_auth`.
    let protected = Router::new()
        // Sessions
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/chat", get(get_chat))
        .route("/chat/reset", post(reset_chat))
        .route(
            "/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route("/sessions/{id}/archive", post(archive_session))
        .route("/sessions/{id}/adopt", post(adopt_session))
        .route("/sessions/{id}/github", post(refresh_github_session))
        .route("/sessions/{id}/raw", get(raw_session))
        // Embedded VS Code (code-server), reverse-proxied per session. `ide-info`
        // is the UI's availability probe; the `ide`/`ide/`/`ide/*` routes serve
        // the editor itself (the static segments win over the `{*rest}`
        // catch-all). The bare `…/ide/` (trailing slash, empty rest) needs its
        // own route: a catch-all does NOT match an empty final segment, so
        // without it the iframe's exact `…/ide/?folder=…` URL would fall through
        // to the SPA index.html and render loom inside its own editor pane.
        .route("/sessions/{id}/ide-info", get(crate::ide::info))
        .route("/sessions/{id}/ide", axum::routing::any(crate::ide::proxy))
        .route("/sessions/{id}/ide/", axum::routing::any(crate::ide::proxy))
        .route(
            "/sessions/{id}/ide/{*rest}",
            axum::routing::any(crate::ide::proxy),
        )
        .route("/sessions/{id}/artifacts", get(list_artifacts))
        .route(
            "/sessions/{id}/artifacts/{name}",
            get(get_artifact)
                .put(write_artifact)
                .delete(delete_artifact),
        )
        .route(
            "/sessions/{id}/scratch",
            get(list_scratch)
                .post(upload_scratch)
                .delete(delete_scratch),
        )
        .route("/sessions/{id}/log", get(log_session))
        .route("/sessions/{id}/conversation", get(conversation_session))
        .route("/sessions/{id}/events", get(events_sse))
        .route("/sessions/{id}/terminal", get(crate::terminal::terminal_ws))
        // Per-session worktree debug shells: `shells` lists the live ones (so the
        // UI re-opens the tabs after a reload), `shell/{idx}/terminal` is the
        // lazily-spawned WebSocket bridge, and DELETE closes one (the tab's ×).
        .route("/sessions/{id}/shells", get(list_session_shells))
        .route(
            "/sessions/{id}/shell/{idx}",
            axum::routing::delete(delete_session_shell),
        )
        .route(
            "/sessions/{id}/shell/{idx}/terminal",
            get(crate::terminal::session_shell_ws),
        )
        // Drive a session's terminal pane: type a message, interrupt, peek at it.
        .route("/sessions/{id}/send", post(send_session))
        .route("/sessions/{id}/interrupt", post(interrupt_session))
        .route("/sessions/{id}/preview", get(preview_session))
        .route(
            "/sessions/{id}/tags/{key}",
            axum::routing::put(set_session_tag).delete(clear_session_tag),
        )
        // Branches & issues
        .route("/branches", get(list_branches))
        .route("/branches/{id}", get(get_branch).patch(patch_branch))
        .route(
            "/branches/{id}/issues",
            get(list_branch_issues).post(create_branch_issue),
        )
        // The cross-repo issue board (the loom Issues pane consumes this).
        .route("/issues", get(list_all_issues))
        .route(
            "/issues/{id}",
            get(get_issue).patch(patch_issue).delete(delete_issue),
        )
        .route(
            "/issues/{id}/tags/{key}",
            axum::routing::put(set_issue_tag).delete(clear_issue_tag),
        )
        // Misc
        .route("/agents", get(list_agents))
        // The managed repo store + clone allowlist (register/list).
        .route("/repos", get(list_repos).post(register_repo))
        .route("/repos/recent", get(recent_repos))
        .route("/repos/branches", get(repo_branches))
        .route(
            "/repos/issues",
            get(list_repo_issues).post(create_repo_issue),
        )
        .route("/settings", get(get_settings).patch(patch_settings))
        // Operator-managed agent environment variables.
        .route("/env", get(get_env))
        .route(
            "/env/{name}",
            axum::routing::put(put_env).delete(delete_env),
        )
        // The operator scratch shell — a single persistent login shell in the
        // container, for one-time setup like `gcloud auth login`.
        .route("/shell/terminal", get(crate::terminal::shell_ws))
        .route("/shell/restart", post(restart_shell))
        // Overlookers — periodic / triggered watch programs over the fleet.
        .route(
            "/overlookers",
            get(list_overlookers).post(create_overlooker),
        )
        // The static segment wins over the `{id}` capture below, so a program
        // named "programs" can't shadow this listing.
        .route("/overlookers/programs", get(list_programs))
        .route(
            "/overlookers/{id}",
            get(get_overlooker)
                .patch(patch_overlooker)
                .delete(delete_overlooker),
        )
        .route("/overlookers/{id}/run", post(run_overlooker))
        .route("/overlookers/{id}/runs", get(overlooker_runs))
        // The one-shot headless agent — the judgement primitive overlooker
        // programs (and any script) call through the daemon.
        .route("/agent/oneshot", post(agent_oneshot))
        // Authentication management: API tokens, the caller's password, the
        // approved-user allowlist, and the GitHub OAuth app config.
        .route("/auth/tokens", get(list_tokens).post(create_token))
        .route("/auth/tokens/{id}", delete(revoke_token))
        .route("/auth/password", post(set_own_password))
        .route("/auth/users", get(list_users).post(add_user))
        .route("/auth/users/{username}", delete(remove_user))
        .route(
            "/auth/github/config",
            get(get_github_config).put(put_github_config),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        // Scratch uploads can carry images / logs; lift the default 2 MB cap.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024));

    let api = public
        .merge(protected)
        // ETag/304 short-circuit for cacheable GETs — applied across the whole
        // API surface (public + protected) before the state is sealed in.
        .layer(axum::middleware::from_fn(api_etag_middleware))
        .with_state(state);

    let index = static_dir().join("index.html");
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(static_dir()).fallback(ServeFile::new(index)))
        .layer(axum::middleware::from_fn(static_cache_middleware))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

/// Query for `GET /api/sessions`: trim the fleet listing for the caller.
#[derive(Debug, Default, Deserialize)]
struct ListSessionsQuery {
    /// Include archived (torn-down) sessions. Defaults to `false` — an archived
    /// session is out of the active fleet, so the agent's `loom session ls` and
    /// any survey see only live work unless they ask. The dashboard, which has
    /// its own "show archived" toggle, opts in with `?archived=true`.
    #[serde(default)]
    archived: bool,
    /// Case-insensitive substring filter over a session's title, branch name,
    /// and goal — so the concierge can narrow a large fleet to the one it wants
    /// (`loom session ls --search auth`). Absent/blank matches everything.
    #[serde(default)]
    q: Option<String>,
}

async fn list_sessions(
    State(st): State<AppState>,
    Query(q): Query<ListSessionsQuery>,
) -> ApiResult<Json<Vec<SessionView>>> {
    // The fleet listing shows work, not infrastructure: engine-managed (warm)
    // sessions are excluded here, so neither the dashboard nor an overlooker
    // round's survey (scripts read this route) ever sees a watcher's own
    // session — the no-recursion guarantee. `list_visible` drops `managed_by`
    // rows; the `warm_session_id` check below is belt-and-braces for a warm
    // session not yet stamped. Internal liveness/adopt paths use
    // `session::list` instead.
    let warm: std::collections::HashSet<String> = ov::list(&st.db)
        .await?
        .into_iter()
        .filter_map(|o| o.warm_session_id)
        .collect();
    // A blank `q` is no filter; otherwise match case-insensitively.
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
    let sessions = session_mod::list_visible(&st.db).await?;
    let mut views: Vec<SessionView> = Vec::with_capacity(sessions.len());
    for s in sessions {
        if warm.contains(&s.id) {
            continue;
        }
        // Archived sessions are torn down — hidden unless the caller opts in.
        if !q.archived && s.status == "archived" {
            continue;
        }
        if let Some(branch) = branch_mod::get(&st.db, &s.branch_id).await? {
            if let Some(needle) = &needle {
                // Match the identifiers a human searches by: the title, the branch
                // name, and the goal.
                let hay =
                    format!("{} {} {}", branch.title, branch.branch, branch.goal).to_lowercase();
                if !hay.contains(needle) {
                    continue;
                }
            }
            views.push(session_view(&st.db, &s, &branch).await?);
        }
    }
    Ok(Json(views))
}

async fn get_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

async fn create_session(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateReq>,
) -> ApiResult<Json<SessionView>> {
    // Attribute the session to whoever the auth middleware resolved: a human
    // (cookie/token) → their username; a loopback/local-token call → the owner;
    // a future webhook → its bot principal. Read from the `Principal`, never
    // hardcoded and never client-supplied.
    Ok(Json(
        create_session_core(st, req, Some(principal.username)).await?,
    ))
}

/// Resolve the **runtime** a session of `agent_kind` launches with. Every kind is
/// its own runtime, except the concierge role-kind, which launches whatever
/// `concierge.runtime` names (claude|codex). Keeps the stored kind (the role)
/// separate from the binary that runs.
async fn launch_runtime(db: &Db, agent_kind: &str) -> String {
    if agent_kind == agent::CONCIERGE_KIND {
        configured_agent(db, "concierge.runtime", config::DEFAULT_CONCIERGE_RUNTIME).await
    } else {
        agent_kind.to_string()
    }
}

async fn configured_agent(db: &Db, key: &str, default: &str) -> String {
    let value = config::get_or(db, key, default).await;
    let value = value.trim();
    if agent::agent_type(value).is_some() {
        value.to_string()
    } else {
        default.to_string()
    }
}

async fn configured_selector(db: &Db, key: &str, runtime: &str, model: bool) -> String {
    let value = config::get_or(db, key, "").await;
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let Some(agent_type) = agent::agent_type(runtime) else {
        return String::new();
    };
    let valid = if model {
        agent_type.validate(value, "").is_ok()
    } else {
        agent_type.validate("", value).is_ok()
    };
    if valid {
        value.to_string()
    } else {
        String::new()
    }
}

/// A freshly launched session's lifecycle status. A Claude runtime starts
/// `launching` because its `SessionStart`/work hook will promote it to `running`;
/// a hookless runtime (shell, codex, a bare command) never gets that hook, so it
/// is `running` from the start rather than stuck `launching`.
fn initial_status(runtime: &str) -> &'static str {
    if agent::starts_from_hook(runtime) {
        "launching"
    } else {
        "running"
    }
}

/// The session-creation core, shared by `POST /api/sessions` and the Chat
/// surface's concierge get-or-create ([`get_chat`]). Returns the view directly so
/// the caller can shape its own response.
///
/// `created_by` is the launching principal's username (attribution for the shared
/// board), or `None` for a system launch with no user behind it (the concierge).
async fn create_session_core(
    st: AppState,
    req: CreateReq,
    created_by: Option<String>,
) -> ApiResult<SessionView> {
    // Resolve the repo root. An explicit managed `repo` (a slug/URL) is
    // allowlist-checked and cloned-if-absent into the managed store, then used
    // directly; otherwise fork from `cwd`'s repo (the default). The traversal /
    // allowlist gate lives in `repo::resolve_clone`.
    let repo_root = match req.repo.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(input) => repo::resolve_clone(&st.db, input)
            .await
            .map_err(|e| match e {
                repo::ResolveError::BadRequest(m) => AppError::bad_request(m),
                repo::ResolveError::Clone(m) => AppError::new(StatusCode::BAD_GATEWAY, m),
            })?,
        None => {
            let cwd = PathBuf::from(&req.cwd);
            git::repo_root(&cwd)
                .await
                .map_err(|e| AppError::bad_request(e.to_string()))?
        }
    };
    // Canonicalize so repo identity matches the `weaver` CLI's resolver — issues
    // are keyed on this path and the two binaries must agree on it.
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);

    let agent = match req.agent {
        Some(a) => a.trim().to_string(),
        None => configured_agent(&st.db, "agent.default", config::DEFAULT_AGENT).await,
    };
    // The concierge is the fleet Chat agent, not a workstream: it gets the
    // fleet-ops primer as its opening prompt and no tracking issue (it has no
    // deliverable to track), and is hidden from the fleet list by its kind.
    let is_concierge = agent == agent::CONCIERGE_KIND;
    let runtime = launch_runtime(&st.db, &agent).await;

    // Normalize and validate the model / effort selections through the resolved
    // agent type. Blank means the agent's own default.
    let configured_model_key = if is_concierge {
        "concierge.model"
    } else {
        "agent.model"
    };
    let configured_effort_key = if is_concierge {
        "concierge.effort"
    } else {
        "agent.effort"
    };
    let model = match req.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => model.to_string(),
        Some(_) => String::new(),
        None => configured_selector(&st.db, configured_model_key, &runtime, true).await,
    };
    let effort = match req.effort.as_deref().map(str::trim) {
        Some(effort) if !effort.is_empty() => effort.to_string(),
        Some(_) => String::new(),
        None => configured_selector(&st.db, configured_effort_key, &runtime, false).await,
    };
    if let Some(agent_type) = agent::agent_type(&runtime) {
        agent_type
            .validate(&model, &effort)
            .map_err(AppError::bad_request)?;
    } else if !model.is_empty() || !effort.is_empty() {
        return Err(AppError::bad_request(format!(
            "custom agent '{runtime}' does not accept model or effort selectors"
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
    let tracking_issue = if is_concierge {
        None
    } else {
        create_tracking_issue(
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
        .await?
    };

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    // Drop any attached reference files into the worktree before the agent
    // launches, then tell the agent they are there. The branch goal stays the
    // clean text the user typed; the scratch and tracking notes ride on the
    // launch prompt (goal.txt) only, so they reach the agent without cluttering
    // the dashboard.
    let scratch_names = write_initial_scratch(&work_dir, &req.scratch).await?;
    // The concierge boots primed-but-idle: its fleet-ops primer is injected as
    // system *context* (primer.txt → `--append-system-prompt-file`), not as a
    // positional opening prompt, so it takes no turn until the operator sends the
    // first message. A normal session's goal/scratch/tracking note ride in as the
    // positional prompt (goal.txt) that seeds its first turn.
    let (goal_file, primer_file) = if is_concierge {
        let f = run_dir.join("primer.txt");
        tokio::fs::write(&f, agent::concierge_primer()).await?;
        (None, Some(f))
    } else {
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
        (goal_file, None)
    };

    let term_session = format!("weaver-{session_id}");
    let extra_env = agent_env::pairs(&st.db).await.unwrap_or_default();
    agent::launch(
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &runtime,
            work_dir: &work_dir,
            term_session: &term_session,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            server_addr: &st.addr,
            model: &model,
            effort: &effort,
            extra_env: &extra_env,
        },
        agent::LaunchMode::Fresh,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Only a Claude runtime emits the hook that promotes `launching` → `running`;
    // a hookless runtime (shell, codex, a bare command) is live on launch.
    let status = initial_status(&runtime);
    let session = session_mod::insert(
        &st.db,
        &NewSession {
            id: session_id.clone(),
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            term_session,
            agent_kind: agent,
            model,
            effort,
            status: status.to_string(),
            github_repo: github_repo.clone(),
            parent_branch_id: parent.as_ref().map(|b| b.id.clone()),
            managed_by: None,
            created_by,
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

    // The concierge boots primed-but-idle: with no positional prompt it takes no
    // turn on launch, so claude's `Stop`/`Notification` hooks never fire and the
    // soothing `idle` mark that those hooks stamp is never set — leaving a freshly
    // booted concierge reading "Working…" forever though it is doing nothing.
    // Stamp the mark ourselves at creation so it reads the calm "Idle" it actually
    // is. The lifecycle then self-heals: the operator's first message fires the
    // `working` hook (which clears this), and each finished turn re-stamps it. A
    // normal session seeds a positional prompt and runs a turn on launch, so its
    // own hooks drive this mark — only the idle-booting concierge needs the seed.
    if is_concierge {
        if let Err(e) = tags::set(
            &st.db,
            &branch.id,
            tags::IDLE_KEY,
            tags::IDLE_VALUE,
            "",
            "agent",
        )
        .await
        {
            tracing::warn!(branch = %branch.id, error = %e, "failed to stamp concierge idle mark");
        } else {
            events::record_tag(
                &st.db,
                &st.bus,
                &branch.id,
                tags::IDLE_KEY,
                tags::IDLE_VALUE,
                "",
                "agent",
            )
            .await
            .ok();
        }
    }

    tracing::info!(
        branch = %branch.id,
        session = %session.id,
        status = %session.status,
        agent = %session.agent_kind,
        "session created"
    );

    let mut view = session_view(&st.db, &session, &branch).await?;
    view.tracking_issue = tracking_issue;
    Ok(view)
}

/// `GET /api/chat` — the Chat surface's concierge. Get-or-create the singleton
/// fleet concierge: return the live one if it exists, else launch a new concierge
/// session in the most-recently-used repo (its home — it doesn't touch the code,
/// but the session machinery needs a worktree). 400 when there is no repo yet.
async fn get_chat(State(st): State<AppState>) -> ApiResult<Json<SessionView>> {
    if let Some(session) = session_mod::active_concierge(&st.db).await? {
        let branch = branch_mod::get(&st.db, &session.branch_id)
            .await?
            .ok_or_else(|| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "branch vanished"))?;
        return Ok(Json(session_view(&st.db, &session, &branch).await?));
    }
    Ok(Json(create_concierge(st).await?))
}

/// Launch a fresh fleet concierge in the most-recently-used repo and return its
/// view. The shared creation path behind [`get_chat`] (when none is live) and
/// [`reset_chat`] (after the old one is archived). 400 when no repo has been
/// used yet — the concierge needs a worktree to live in.
async fn create_concierge(st: AppState) -> ApiResult<SessionView> {
    let home = repo::recent(&st.db, 1)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            AppError::bad_request(
                "the concierge needs a repo to live in — open a session in a repo first",
            )
        })?;
    let req = CreateReq {
        cwd: home.repo_root,
        agent: Some(agent::CONCIERGE_KIND.to_string()),
        title: Some("Fleet concierge".to_string()),
        ..Default::default()
    };
    // The concierge is a system singleton, not a user's workstream (and is hidden
    // from the fleet board), so it carries no creator attribution.
    create_session_core(st, req, None).await
}

/// `POST /api/chat/reset` — start a clean conversation with the concierge. The
/// live concierge (if any) is archived — its terminal and worktree torn down,
/// its transcript captured to history — and a brand-new one is launched in its
/// place, so the operator gets a fresh agent with none of the prior context.
/// Returns the new session view, exactly as [`get_chat`] would.
async fn reset_chat(State(st): State<AppState>) -> ApiResult<Json<SessionView>> {
    if let Some(session) = session_mod::active_concierge(&st.db).await? {
        if let Some(branch) = branch_mod::get(&st.db, &session.branch_id).await? {
            // Best-effort teardown of the old concierge; a warning here must not
            // block the fresh start the operator asked for.
            if let Err(e) = archive(&st, &session, &branch).await {
                tracing::warn!(session = %session.id, error = ?e, "reset: archiving old concierge failed");
            }
        }
    }
    Ok(Json(create_concierge(st).await?))
}

/// A line appended to a session's launch prompt telling the agent which weaver
/// issue tracks its task, so it keeps the issue up to date and closes it when
/// done. Mirrors [`scratch_note`]: it rides on the prompt only, never the
/// stored goal.
fn tracking_note(issue_id: i64) -> String {
    format!(
        "This session is tracked as weaver issue #{issue_id}. Keep your status \
         current with `weaver status <level> \"<message>\"` as you work, and \
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

/// The author of a mutation: the trimmed `by`, or `manual` when absent or
/// all-whitespace (an empty author never reaches the audit trail).
fn author_or_manual(by: Option<&str>) -> String {
    by.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("manual")
        .to_string()
}

/// Set (upsert) a tag on a session's branch: validate `value` against the key's
/// ladder, write the tag, and broadcast a `tag` event. The well-known keys are
/// `attention` (the agent's self-report) and `triage` (an overlooker's, or a
/// hand operator's, assessment); any other key is a free-form quiet pill. To
/// return a loud key to calm, `DELETE` the tag rather than setting an `ok` value.
async fn set_session_tag(
    State(st): State<AppState>,
    Path((key, tag_key)): Path<(String, String)>,
    Json(req): Json<TagReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let value = req.value.trim();
    if !tags::is_valid_value(&tag_key, value) {
        return Err(AppError::bad_request(if tags::is_loud(&tag_key) {
            format!(
                "invalid value '{value}' for '{tag_key}' — expected one of {} (clear the tag to return to calm)",
                tags::ATTENTION_VALUES.join(", ")
            )
        } else {
            format!("invalid value '{value}' for '{tag_key}' — must be non-empty")
        }));
    }
    let by = author_or_manual(req.by.as_deref());
    let note = req.note.trim();
    tags::set(&st.db, &branch.id, &tag_key, value, note, &by).await?;
    events::record_tag(&st.db, &st.bus, &branch.id, &tag_key, value, note, &by)
        .await
        .ok();
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Clear a tag on a session's branch — delete the row and broadcast a `tag`
/// event with an empty value (the cleared signal). How a loud axis returns to
/// calm (`ok`). A no-op when the tag is already absent. DELETE carries no
/// body, so the author rides the `by` query parameter (an overlooker name),
/// defaulting to `manual`.
async fn clear_session_tag(
    State(st): State<AppState>,
    Path((key, tag_key)): Path<(String, String)>,
    Query(q): Query<ByQuery>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let by = author_or_manual(q.by.as_deref());
    tags::clear(&st.db, &branch.id, &tag_key).await?;
    events::record_tag(&st.db, &st.bus, &branch.id, &tag_key, "", "", &by)
        .await
        .ok();
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Query string carrying the author of a body-less mutation (a tag DELETE).
#[derive(Debug, Deserialize)]
struct ByQuery {
    #[serde(default)]
    by: Option<String>,
}

async fn patch_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchSessionReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
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
    Ok(Json(session_view(&st.db, &session, &branch).await?))
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

    backend::kill_session(&session.term_session).await.ok();
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
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

/// Archive a session: tear down its terminal and remove the worktree, but keep the
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

    // Capture the agent's conversation log before teardown. The transcript lives
    // outside the worktree so it would survive removal, but capturing first keeps
    // it whole regardless. Best-effort: failures are warnings, never fatal.
    let (_, log_warnings) = crate::chatlog::capture(&st.db, session, branch).await;
    warnings.extend(log_warnings);

    backend::kill_session(&session.term_session).await.ok();
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
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
    // longer "need me" — nor is it "resting". Clear every loud tag — the agent's
    // own `attention` and any watch's typed marks (loudness is value-driven, so
    // match on the value, not a fixed key set) — plus the soothing `idle` mark,
    // so the dashboard stops flagging or labelling a torn-down workstream —
    // absence is the calm state. The history (goal, status, events) is kept; the
    // `description` message stays too, as do any free-form quiet pills.
    for tag in tags::list(&st.db, &branch.id).await? {
        if tags::is_loud_value(&tag.value) || tag.key == tags::IDLE_KEY {
            tags::clear(&st.db, &branch.id, &tag.key).await?;
            events::record_tag(&st.db, &st.bus, &branch.id, &tag.key, "", "", "manual")
                .await
                .ok();
        }
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

/// `GET /api/sessions/{id}/shells` — the live worktree debug-shell indices for a
/// session, so the UI re-opens the shell tabs after a reload (the shells are
/// detached supervisors that outlive the page). Never spawns.
async fn list_session_shells(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<u32>>> {
    let (session, _) = require_session(&st.db, &key).await?;
    Ok(Json(crate::shell::list_debug(&session.id).await))
}

/// `DELETE /api/sessions/{id}/shell/{idx}` — close one worktree debug shell (the
/// tab's ×), killing its supervisor. Idempotent: a missing shell is a no-op.
async fn delete_session_shell(
    State(st): State<AppState>,
    Path((key, idx)): Path<(String, u32)>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    crate::shell::kill_debug(&session.id, idx).await;
    Ok(Json(json!({ "closed": true })))
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
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Bring up an engine-managed (warm) session for an overlooker, reusing the same
/// branch/worktree/terminal launch machinery as an ordinary session — the only
/// differences are that it forks a dedicated `weaver/overlooker-<name>` branch
/// and the row is stamped `managed_by = overlooker.id` so the fleet listing and
/// every survey hide it.
///
/// A warm session is the watcher's own long-lived agent; its persistence across
/// rounds (the same terminal/worktree, resumed on adopt) is what gives the overlooker
/// across-round memory. The engine calls this once, on first need
/// ([`crate::overlooker::ensure_warm_session`]); thereafter it reuses the stored
/// session id.
pub async fn create_warm_session(
    st: &AppState,
    overlooker: &Overlooker,
    repo_root: &std::path::Path,
) -> Result<Session, AppError> {
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let repo_root_str = repo_root.display().to_string();
    let base = git::default_base(&repo_root).await?;

    // A stable, collision-resistant branch slug per overlooker; if an old warm
    // branch lingers (a prior warm session was archived), suffix to a fresh one.
    let base_slug = format!("overlooker-{}", branch_mod::slugify(&overlooker.name));
    let mut slug = base_slug.clone();
    let mut suffix = 2;
    loop {
        let branch_name = format!("weaver/{slug}");
        let dir = repo_root.join(".worktrees").join(&slug);
        if !git::branch_exists(&repo_root, &branch_name).await && !dir.exists() {
            break;
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

    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    branch_mod::set_title(
        &st.db,
        &branch.id,
        &format!("overlooker {}", overlooker.name),
    )
    .await?;

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    // The warm session runs the configured default agent (the overlooker's
    // judging agent, normally `claude`); its `prompt` param, when set, seeds the
    // first turn.
    let agent = configured_agent(&st.db, "agent.default", config::DEFAULT_AGENT).await;
    let goal_file = match overlooker
        .params()
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        Some(prompt) => {
            let f = run_dir.join("goal.txt");
            tokio::fs::write(&f, prompt).await?;
            Some(f)
        }
        None => None,
    };

    let term_session = format!("weaver-{session_id}");
    let extra_env = agent_env::pairs(&st.db).await.unwrap_or_default();
    // A warm session never carries the concierge role, so its runtime is its kind.
    agent::launch(
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &agent,
            work_dir: &work_dir,
            term_session: &term_session,
            goal_file: goal_file.as_deref(),
            // A warm overlooker session is never the concierge, so no primer.
            primer_file: None,
            server_addr: &st.addr,
            model: &overlooker.model,
            effort: &overlooker.effort,
            extra_env: &extra_env,
        },
        agent::LaunchMode::Fresh,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let status = initial_status(&agent);
    let session = session_mod::insert(
        &st.db,
        &NewSession {
            id: session_id,
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            term_session,
            agent_kind: agent,
            model: overlooker.model.clone(),
            effort: overlooker.effort.clone(),
            status: status.to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(overlooker.id.clone()),
            // Engine-created infrastructure, no user behind it.
            created_by: None,
        },
    )
    .await?;

    repo::record_use(&st.db, &repo_root_str).await.ok();
    tracing::info!(
        overlooker = %overlooker.id,
        session = %session.id,
        "warm session created"
    );
    Ok(session)
}

/// Recreate an orphaned session's terminal and resume its agent.
pub async fn adopt(st: &AppState, session: &Session, branch: &Branch) -> Result<(), AppError> {
    if backend::has_session(&session.term_session).await {
        return Err(AppError::conflict(
            "session already has a running terminal process",
        ));
    }
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(AppError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            session.work_dir
        )));
    }
    // A normal session persists its positional prompt in goal.txt; the concierge
    // persists its primer in primer.txt instead (re-appended as system context so
    // it stays primed-but-idle across adopt). Each session carries exactly one.
    let run_dir = db::run_dir(&session.id);
    let primer_file = {
        let f = run_dir.join("primer.txt");
        f.exists().then_some(f)
    };
    let goal_file = {
        let f = run_dir.join("goal.txt");
        f.exists().then_some(f)
    };
    let extra_env = agent_env::pairs(&st.db).await.unwrap_or_default();
    let runtime = launch_runtime(&st.db, &session.agent_kind).await;
    agent::launch(
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &runtime,
            work_dir: &work_dir,
            term_session: &session.term_session,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            extra_env: &extra_env,
        },
        agent::LaunchMode::Adopt,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // A hookless runtime (codex, a bare command) won't get the hook that would
    // promote `launching` → `running`, so mark it live now rather than stranding it.
    let status = initial_status(&runtime);
    session_mod::set_status(&st.db, &session.id, status).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": "session adopted" }),
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
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

// ---------------------------------------------------------------------------
// Raw worktree bytes — serves a single file's bytes (with a guessed content
// type) for Markdown inline images. The embedded editor ([`crate::ide`]) is the
// file browsing/editing surface; this endpoint only reads, never writes.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents stored in weaver.db. The GET resolves
// the content's references against the issue ledger (via smartdoc) and returns
// the projection alongside, so the SPA chips and `weaver artifact show` render
// the same join. Structure in the doc, state in the DB. See docs/artifacts.md.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RevQuery {
    rev: Option<i64>,
}

/// The wire metadata for an artifact envelope.
fn artifact_meta(a: &Artifact) -> ArtifactMeta {
    ArtifactMeta {
        id: a.id,
        name: a.name.clone(),
        kind: a.kind.clone(),
        title: a.title.clone(),
        branch_id: a.branch_id.clone(),
        rev: a.rev,
        created_at: a.created_at.clone(),
        updated_at: a.updated_at.clone(),
    }
}

/// List the artifacts visible from a session: its branch's plus the repo-shared
/// ones, latest rev each (a branch-scoped name shadows a shared one).
async fn list_artifacts(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<ArtifactMeta>>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let artifacts = artifact::list_for_session(&st.db, &branch.repo_root, &branch.id).await?;
    Ok(Json(artifacts.iter().map(artifact_meta).collect()))
}

/// Resolve an artifact's content references to their live status, as the wire
/// [`ArtifactRefs`]. Probes each `#N` against the repo's issue ledger and joins
/// via [`smartdoc::project`]; an unresolved reference is omitted from the map.
async fn project_artifact_refs(db: &Db, repo_root: &str, content: &str) -> ArtifactRefs {
    let doc = smartdoc::parse(content);
    // Probe each distinct reference against weaver-core. Best-effort: a probe
    // miss (unknown issue, wrong repo, read error) just leaves that ref absent
    // from the status map, which `project` renders as a muted, non-existent chip.
    let mut status: HashMap<smartdoc::Ref, smartdoc::RefStatus> = HashMap::new();
    for r in smartdoc::refs(&doc) {
        if let smartdoc::Ref::Issue(n) = &r {
            if let Ok(Some(issue)) = weaver_core::issue::get(db, *n as i64).await {
                if issue.repo_root == repo_root {
                    status.insert(
                        r.clone(),
                        smartdoc::RefStatus {
                            exists: true,
                            title: issue.title,
                            status: issue.status,
                            claimed_branch: issue.claimed_branch,
                        },
                    );
                }
            }
        }
    }
    // Join, then shape the resolved issue refs into the wire map (keyed by id).
    let mut refs = ArtifactRefs::default();
    for pr in smartdoc::project(&doc, &status).refs {
        if let smartdoc::Ref::Issue(n) = pr.reference {
            if pr.status.exists {
                refs.issues.insert(
                    n.to_string(),
                    IssueRefStatus {
                        id: n as i64,
                        title: pr.status.title,
                        status: pr.status.status,
                        claimed_branch: pr.status.claimed_branch,
                    },
                );
            }
        }
    }
    refs
}

/// Build the full [`ArtifactView`] for an artifact at a given revision (default
/// latest): envelope, content, version list, and the projected reference map.
async fn artifact_view(
    db: &Db,
    repo_root: &str,
    a: &Artifact,
    rev: Option<i64>,
) -> ApiResult<ArtifactView> {
    let version = match rev {
        Some(r) => artifact::version(db, a.id, r).await?,
        None => artifact::latest_version(db, a.id).await?,
    }
    .ok_or_else(|| AppError::not_found("artifact revision"))?;
    let versions = artifact::history(db, a.id)
        .await?
        .into_iter()
        .map(|v| weaver_api::ArtifactVersion {
            rev: v.rev,
            author: v.author,
            created_at: v.created_at,
        })
        .collect();
    let refs = project_artifact_refs(db, repo_root, &version.content).await;
    Ok(ArtifactView {
        meta: artifact_meta(a),
        content: version.content,
        versions,
        refs,
    })
}

/// One artifact, content + projected refs, resolving branch-scoped before
/// repo-shared. `?rev=N` selects a revision; the default is latest.
async fn get_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Query(q): Query<RevQuery>,
) -> ApiResult<Json<ArtifactView>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, q.rev).await?,
    ))
}

/// Write a new revision of an artifact (a user edit, `author: user`), returning
/// the refreshed view at the new latest revision. The artifact must already
/// exist in the session's view; the write targets the resolved scope (its own
/// branch-scoped row, else the repo-shared one).
async fn write_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<ArtifactWriteBody>,
) -> ApiResult<Json<ArtifactView>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let existing = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    // Keep the existing kind/title unless the body overrides them.
    let kind = body.kind.unwrap_or_else(|| existing.kind.clone());
    let title = body.title.unwrap_or_else(|| existing.title.clone());
    // Write into the same scope the artifact resolved to (a shared artifact
    // edited from a session writes a new shared revision, not a branch copy).
    let scope = existing.branch_id.as_deref();
    let a = artifact::write(
        &st.db,
        &artifact::NewRevision {
            repo_root: &branch.repo_root,
            branch_id: scope,
            name: &name,
            kind: &kind,
            title: &title,
            content: &body.content,
            author: "user",
        },
    )
    .await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_written",
        json!({ "name": a.name, "rev": a.rev, "title": a.title }),
    )
    .await
    .ok();
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, None).await?,
    ))
}

/// Delete an artifact and its whole revision history. Resolves the name the way
/// the session sees it (its own branch-scoped row, else the repo-shared one — the
/// single row the list shows for that name), so deleting from the UI removes
/// exactly the artifact displayed. Broadcasts `artifact_deleted` for live refresh.
async fn delete_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    artifact::delete(&st.db, a.id).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_deleted",
        json!({ "name": a.name, "branch_id": a.branch_id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "deleted": true, "name": a.name })))
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

/// The session's agent conversation as a normalized iris log — the live
/// transcript when present, else the capture archived alongside it. 404 when the
/// session has no conversation (e.g. a `shell` session, or none recorded yet).
async fn conversation_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<weaver_core::transcript::Log>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    match crate::chatlog::conversation(&st.db, &session, &branch).await {
        Some(log) => Ok(Json(log)),
        None => Err(AppError::not_found("conversation")),
    }
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
// Driving a session's terminal pane (send / interrupt / preview)
//
// One-shot HTTP primitives for an agent (or script) to drive a child session
// uniformly, distinct from the interactive terminal WebSocket: type a message,
// interrupt the current turn, or read back the pane.
// ---------------------------------------------------------------------------

/// Guard the pane-driving endpoints: the session must have a live terminal to type
/// into or capture. An orphaned/torn-down session returns 409.
async fn require_live_terminal(session: &Session) -> ApiResult<()> {
    if backend::has_session(&session.term_session).await {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "session '{}' has no live terminal to drive",
            session.id
        )))
    }
}

/// Type a message into a session's agent pane and, by default, submit it with
/// Enter to trigger an agent round. Every send is also a `nudge` events row
/// (the audit rule — every mutating action is an events row), attributed to
/// `by` (an overlooker name, or `manual` when absent).
async fn send_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    require_live_terminal(&session).await?;
    backend::send_literal(&session.term_session, &req.text)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if req.submit {
        backend::send_enter(&session.term_session)
            .await
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let by = author_or_manual(req.by.as_deref());
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({ "by": by, "text": req.text }),
    )
    .await
    .ok();
    Ok(Json(json!({ "sent": true, "submitted": req.submit })))
}

/// Send a break/interrupt — `Escape`, the keystroke Claude Code reads as "stop
/// the current turn" — to a session's agent pane.
async fn interrupt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_live_terminal(&session).await?;
    backend::send_key(&session.term_session, "Escape")
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

/// Capture the session's terminal pane as plain text — "what does the child look
/// like right now". Returns `{ "screen": "<text>" }`.
async fn preview_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<PreviewQuery>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_live_terminal(&session).await?;
    let screen = backend::capture(&session.term_session, q.lines)
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
        out.push(branch_view(&st.db, &b).await?);
    }
    Ok(Json(out))
}

async fn get_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    Ok(Json(branch_view(&st.db, &branch).await?))
}

#[derive(Debug, Deserialize)]
struct PatchBranchReq {
    title: Option<String>,
    goal: Option<String>,
    description: Option<String>,
}

async fn patch_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchBranchReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
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
    Ok(Json(branch_view(&st.db, &branch).await?))
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct IssueListQuery {
    #[serde(default)]
    all: bool,
}

/// Build an [`IssueView`] for an issue, gathering its tags (a separate query).
async fn issue_view(db: &Db, issue: Issue) -> ApiResult<IssueView> {
    let tags = weaver_core::issue::list_tags(db, issue.id).await?;
    Ok(IssueView::from_parts(issue, &tags))
}

/// Build views for a batch of issues, each with its tags joined.
async fn issue_views(db: &Db, issues: Vec<Issue>) -> ApiResult<Vec<IssueView>> {
    let mut out = Vec::with_capacity(issues.len());
    for i in issues {
        out.push(issue_view(db, i).await?);
    }
    Ok(out)
}

/// Every issue across every repo — the loom dashboard's cross-repo issue board.
async fn list_all_issues(
    State(st): State<AppState>,
    Query(q): Query<IssueListQuery>,
) -> ApiResult<Json<Vec<IssueView>>> {
    let issues = weaver_core::issue::list_all(&st.db, q.all).await?;
    Ok(Json(issue_views(&st.db, issues).await?))
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
    Ok(Json(issue_views(&st.db, issues).await?))
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
    Ok(Json(issue_view(&st.db, issue).await?))
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
    Ok(Json(issue_view(&st.db, issue).await?))
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
    Ok(Json(issue_view(&st.db, issue).await?))
}

/// Set (upsert) a free-form label on an issue. Issue tags carry no loud
/// `attention`/`triage` ladder — every key is a quiet annotation, so the only
/// rule is a non-empty value (clear the tag with `DELETE` to remove a label). A
/// `tag` event is recorded on the branch working the issue, when there is one,
/// so its session feed refreshes.
async fn set_issue_tag(
    State(st): State<AppState>,
    Path((id, tag_key)): Path<(i64, String)>,
    Json(req): Json<TagReq>,
) -> ApiResult<Json<IssueView>> {
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    let key = tag_key.trim();
    let value = req.value.trim();
    if key.is_empty() {
        return Err(AppError::bad_request("tag key is required"));
    }
    if value.is_empty() {
        return Err(AppError::bad_request(format!(
            "invalid value for '{key}' — must be non-empty (clear the tag to remove it)"
        )));
    }
    let by = author_or_manual(req.by.as_deref());
    let note = req.note.trim();
    weaver_core::issue::set_tag(&st.db, id, key, value, note, &by).await?;
    if let Some(branch_id) = issue_event_branch(&st.db, &issue).await {
        events::record(
            &st.db,
            &st.bus,
            &branch_id,
            "issue_tagged",
            json!({ "id": id, "key": key, "value": value }),
        )
        .await
        .ok();
    }
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    Ok(Json(issue_view(&st.db, issue).await?))
}

/// Clear a label on an issue — delete the `(issue_id, key)` row. A no-op when
/// the tag is already absent.
async fn clear_issue_tag(
    State(st): State<AppState>,
    Path((id, tag_key)): Path<(i64, String)>,
) -> ApiResult<Json<IssueView>> {
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    weaver_core::issue::clear_tag(&st.db, id, tag_key.trim()).await?;
    if let Some(branch_id) = issue_event_branch(&st.db, &issue).await {
        events::record(
            &st.db,
            &st.bus,
            &branch_id,
            "issue_tagged",
            json!({ "id": id, "key": tag_key.trim(), "value": "" }),
        )
        .await
        .ok();
    }
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    Ok(Json(issue_view(&st.db, issue).await?))
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
    Ok(Json(issue_views(&st.db, issues).await?))
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
    Ok(Json(issue_view(&st.db, issue).await?))
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

/// `GET /api/repos` — the registered managed repos (the clone allowlist).
async fn list_repos(State(st): State<AppState>) -> ApiResult<Json<Vec<repo::ManagedRepo>>> {
    Ok(Json(repo::list_registered(&st.db).await?))
}

/// Body for `POST /api/repos`: a repo reference — a GitHub `owner/name` slug or a
/// clone URL — to add to the managed store / allowlist.
#[derive(Debug, Deserialize)]
struct RegisterRepoReq {
    repo: String,
}

/// `POST /api/repos` — register a repo in the managed store. The reference is
/// parsed to a clean `owner/name` slug (traversal rejected → 400); the clone URL
/// is the canonical GitHub HTTPS remote for a bare slug, or the URL as given.
/// The clone itself is lazy — it happens on first use (session create),
/// idempotently — so registering is just adding to the allowlist.
async fn register_repo(
    State(st): State<AppState>,
    Json(req): Json<RegisterRepoReq>,
) -> ApiResult<Json<repo::ManagedRepo>> {
    let slug = repo::parse_slug(&req.repo).map_err(AppError::bad_request)?;
    let remote_url = repo::remote_url_for(&req.repo, &slug);
    let path = slug.path(&repo::repos_dir());
    let managed =
        repo::register(&st.db, &slug.slug(), &remote_url, &path.to_string_lossy()).await?;
    Ok(Json(managed))
}

/// `POST /api/github/webhook` — the inbound GitHub trigger (shared-loom design
/// §6.3). **Public** (outside `require_auth`): every delivery is authenticated by
/// the HMAC signature GitHub carries on it, not by a loom principal. This handler
/// is the untrusted-input boundary; it sequences the gates implemented in
/// [`crate::github_trigger`].
///
/// Status discipline: a missing/invalid signature is a hard **401** (a real
/// misconfiguration GitHub should surface as a failed delivery). Two further
/// non-2xx cases past that are deliberate, not no-ops: a delivery with no
/// `X-GitHub-Delivery` GUID is malformed (**400** — without it idempotency is
/// impossible), and a failure to record the delivery is transient (**5xx**, so
/// GitHub *should* retry). Every *business-logic* outcome — a non-trigger
/// comment, a replay, an unauthorized commenter, a non-allowlisted repo, a
/// rate-limited repo — returns **200**, so GitHub does not retry a delivery we
/// deliberately ignored.
async fn github_webhook(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // 1. Authenticate the delivery: HMAC-SHA256 over the RAW body bytes (never a
    //    re-serialized parse). An empty secret means the webhook is unconfigured,
    //    so it cannot verify anything — reject.
    let secret = github_trigger::webhook_secret(&st.db).await;
    if secret.is_empty() {
        tracing::warn!("github webhook hit but no webhook secret is configured");
        return (StatusCode::UNAUTHORIZED, "webhook not configured").into_response();
    }
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());
    if !github_trigger::verify_signature(&secret, &body, sig) {
        tracing::warn!("github webhook signature verification failed");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }

    // The body is now trusted (GitHub-signed). Every *business-logic* outcome
    // past here is a 200 no-op via `ok()`; the only non-2xx exceptions below are
    // a malformed delivery (no GUID → 400) and a transient store error (→ 5xx).
    let ok = || (StatusCode::OK, "ok").into_response();

    // 2. Idempotency: dedupe on the delivery GUID. A genuine GitHub delivery
    //    always carries one; its absence is a malformed request we reject (400),
    //    since without it idempotency is impossible. A repeat GUID is a no-op.
    let Some(delivery) = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("github webhook missing X-GitHub-Delivery");
        return (StatusCode::BAD_REQUEST, "missing delivery id").into_response();
    };
    match github_trigger::record_delivery(&st.db, delivery).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(delivery, "github webhook: duplicate delivery ignored");
            return ok();
        }
        Err(e) => {
            tracing::error!(error = %e, "github webhook: recording delivery failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "delivery store error").into_response();
        }
    }

    // 3. Filter to issue_comment / created. Other events (incl. the setup `ping`)
    //    and edited/deleted comments are acknowledged and ignored.
    if headers.get("x-github-event").and_then(|v| v.to_str().ok()) != Some("issue_comment") {
        return ok();
    }
    let event = match github_trigger::IssueCommentEvent::parse(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "github webhook: unparseable issue_comment payload");
            return ok();
        }
    };
    if event.action != "created" {
        return ok();
    }

    // 4. Ignore the bot's own comments (no self-trigger loop), then require the
    //    fixed command prefix.
    let author = event.comment.user.login.trim().to_string();
    if let Some(bot) = github_trigger::bot_login(&st.db).await {
        if author.eq_ignore_ascii_case(&bot) {
            return ok();
        }
    }
    let phrase = github_trigger::trigger_phrase(&st.db).await;
    if !github_trigger::is_trigger(&event.comment.body, &phrase) {
        return ok();
    }

    // Validate the repo identifier (defence — it is GitHub's, but the on-disk
    // path derives from it) and split it into owner/name.
    let slug = match repo::parse_slug(&event.repository.full_name) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(repo = %event.repository.full_name, error = %e, "github webhook: bad repo slug");
            return ok();
        }
    };

    // 5. Rate-limit per repo BEFORE the (costly) authorization API call, so a
    //    comment flood cannot fan out into unbounded GitHub calls and launches.
    if !st.trigger.check_rate_limit(&slug.slug()) {
        tracing::warn!(repo = %slug.slug(), "github webhook: per-repo rate limit hit, dropping");
        return ok();
    }

    // 6. Authorize the commenter (the untrusted boundary): a known loom operator,
    //    or a write/admin collaborator on the repo. Unauthorized → silent no-op;
    //    replying would amplify spam across a flood of comments.
    if !github_trigger::authorize(&st.db, st.trigger.gh(), &slug.owner, &slug.name, &author).await {
        tracing::info!(login = %author, repo = %slug.slug(), "github webhook: commenter not authorized");
        return ok();
    }

    // 6a. A repo the GitHub App is installed on is implicitly authorized to
    //     trigger (design §6.3): auto-register it into the managed allowlist so
    //     the clone path below accepts it, *complementing* the explicitly
    //     registered repos from #95. A no-op when the App is unconfigured, the
    //     repo is already registered, or the App is not installed on it — so the
    //     v1 repos-table allowlist still governs the ambient-`GH_TOKEN` flow.
    if let Some(app) = st.trigger.app() {
        app.ensure_installed_repo_registered(&slug).await;
    }

    // 7. Acquire the repo (it must be in the managed allowlist — `resolve_clone`
    //    rejects others) and create the session, seeded from the issue carried in
    //    the trusted payload. Seeding title/goal from the payload (not a `gh`
    //    re-fetch) means the managed clone needs no reachable GitHub remote.
    //    Attribute the launch to the webhook bot, annotated with the triggering
    //    GitHub login (`created_by`, from PR #94) — tracking, not a boundary.
    let req = CreateReq {
        repo: Some(slug.slug()),
        title: Some(event.issue.title.clone()),
        goal: Some(event.goal_seed()),
        ..Default::default()
    };
    let created_by = format!("github-webhook ({author})");
    let view = match create_session_core(st.clone(), req, Some(created_by)).await {
        Ok(v) => v,
        Err(e) => {
            // A non-allowlisted repo lands here (a BadRequest from resolve_clone),
            // as does a clone/launch failure. Acknowledge; don't make GitHub retry.
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: session create failed");
            return ok();
        }
    };

    // 8. Reply on the issue with the live session URL.
    let base = external_base(&st, &headers)
        .await
        .unwrap_or_else(|| format!("http://{}", st.addr));
    let reply = format!("On it — {base}/s/{}", view.id);
    if let Err(e) = st
        .trigger
        .gh()
        .post_issue_comment(&slug.slug(), event.issue.number, &reply)
        .await
    {
        tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting reply failed");
    }
    tracing::info!(
        session = %view.id,
        repo = %slug.slug(),
        issue = event.issue.number,
        login = %author,
        "github webhook: launched session"
    );
    ok()
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
    if errors.is_empty() {
        validate_agent_settings_patch(&st.db, &mut changes, &mut errors).await;
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

fn change_for<'a>(changes: &'a [config::Change], key: &str) -> Option<&'a Option<String>> {
    changes
        .iter()
        .rev()
        .find_map(|(k, v)| (k == key).then_some(v))
}

fn changed(changes: &[config::Change], key: &str) -> bool {
    change_for(changes, key).is_some()
}

async fn setting_after(db: &Db, changes: &[config::Change], key: &str, default: &str) -> String {
    match change_for(changes, key) {
        Some(Some(value)) => value.trim().to_string(),
        Some(None) => default.to_string(),
        None => config::get_or(db, key, default).await.trim().to_string(),
    }
}

async fn validate_agent_settings_patch(
    db: &Db,
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
) {
    validate_agent_settings_group(
        db,
        changes,
        errors,
        AgentSettingsGroup {
            agent_key: "agent.default",
            agent_default: config::DEFAULT_AGENT,
            model_key: "agent.model",
            effort_key: "agent.effort",
            require_concierge: false,
        },
    )
    .await;
    validate_agent_settings_group(
        db,
        changes,
        errors,
        AgentSettingsGroup {
            agent_key: "concierge.runtime",
            agent_default: config::DEFAULT_CONCIERGE_RUNTIME,
            model_key: "concierge.model",
            effort_key: "concierge.effort",
            require_concierge: true,
        },
    )
    .await;
}

struct AgentSettingsGroup {
    agent_key: &'static str,
    agent_default: &'static str,
    model_key: &'static str,
    effort_key: &'static str,
    require_concierge: bool,
}

async fn validate_agent_settings_group(
    db: &Db,
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
    group: AgentSettingsGroup,
) {
    if !changed(changes, group.agent_key)
        && !changed(changes, group.model_key)
        && !changed(changes, group.effort_key)
    {
        return;
    }

    let agent_kind = setting_after(db, changes, group.agent_key, group.agent_default).await;
    let Some(agent_type) = agent::agent_type(&agent_kind) else {
        errors.insert(
            group.agent_key.to_string(),
            json!(format!("unknown agent '{agent_kind}'")),
        );
        return;
    };
    let metadata = agent_type.metadata();
    if group.require_concierge && !metadata.supports_concierge {
        errors.insert(
            group.agent_key.to_string(),
            json!(format!("agent '{agent_kind}' cannot run the concierge")),
        );
        return;
    }

    let model = setting_after(db, changes, group.model_key, "").await;
    validate_agent_selector_setting(
        changes,
        errors,
        group.agent_key,
        group.model_key,
        &model,
        || agent_type.validate(&model, ""),
    );

    let effort = setting_after(db, changes, group.effort_key, "").await;
    validate_agent_selector_setting(
        changes,
        errors,
        group.agent_key,
        group.effort_key,
        &effort,
        || agent_type.validate("", &effort),
    );
}

fn validate_agent_selector_setting(
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
    agent_key: &str,
    selector_key: &str,
    value: &str,
    validate: impl FnOnce() -> std::result::Result<(), String>,
) {
    if value.is_empty() {
        return;
    }
    if let Err(why) = validate() {
        if changed(changes, agent_key) && !changed(changes, selector_key) {
            changes.push((selector_key.to_string(), None));
        } else {
            errors.insert(selector_key.to_string(), json!(why));
        }
    }
}

// ---------------------------------------------------------------------------
// Agent environment variables
// ---------------------------------------------------------------------------

async fn env_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "env": agent_env::list(db).await? })))
}

async fn get_env(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    env_envelope(&st.db).await
}

#[derive(serde::Deserialize)]
struct PutEnvBody {
    value: String,
}

/// Upsert one variable. The name comes from the path; the body carries the
/// value. The name is validated as a shell identifier so it can't corrupt the
/// launch script that exports it; the value is free-form.
async fn put_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PutEnvBody>,
) -> ApiResult<Json<Value>> {
    if let Err(why) = agent_env::validate_name(&name) {
        return Err(AppError::bad_request(why));
    }
    agent_env::set(&st.db, &name, &body.value).await?;
    env_envelope(&st.db).await
}

/// Delete one variable. Returns the refreshed list; a missing name is not an
/// error (the desired end state — absent — already holds).
async fn delete_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    agent_env::remove(&st.db, &name).await?;
    env_envelope(&st.db).await
}

/// `POST /api/shell/restart` — reset the operator scratch shell, killing the
/// current supervisor and spawning a fresh one. Handy after editing operator env
/// vars (the new shell picks them up) or to clear a wedged session.
async fn restart_shell(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    crate::shell::restart(&st).await?;
    Ok(Json(json!({ "restarted": true })))
}

// ===========================================================================
// Authentication
//
// Three credentials resolve to one `auth::Principal`: an `Authorization: Bearer`
// API token, a login session cookie, or a trusted-loopback request. The
// `require_auth` middleware enforces this on every route except the public login
// surface (`/auth/me`, `/auth/login`, `/auth/logout`, `/auth/github/*`) and
// `/health`. The crypto and storage live in `crate::auth`; this is the HTTP glue.
// ===========================================================================

/// The login cookie's `Max-Age` in seconds, derived from the stored-session TTL
/// so the cookie and the server-side expiry can't drift apart.
const SESSION_MAX_AGE: i64 = auth::SESSION_TTL_DAYS * 24 * 60 * 60;
/// The short-lived cookie carrying the OAuth CSRF state across the round-trip.
const OAUTH_STATE_COOKIE: &str = "loom_oauth_state";
/// The GitHub OAuth callback path — the redirect URI registered on the app and
/// reported to the settings UI.
const GITHUB_CALLBACK_PATH: &str = "/api/auth/github/callback";

fn unauthorized(message: &str) -> AppError {
    AppError::new(StatusCode::UNAUTHORIZED, message)
}

/// Pull the token out of an `Authorization: Bearer <token>` header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Read one cookie value by name out of the `Cookie` request header.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// Resolve the caller to an authenticated [`Principal`], or `None`. Order: a
/// bearer token, a session cookie, then loopback trust.
async fn resolve_principal(st: &AppState, headers: &HeaderMap, peer: IpAddr) -> Option<Principal> {
    if let Some(token) = bearer_token(headers) {
        if let Ok(Some(p)) = auth::lookup_token(&st.db, &token).await {
            return Some(p);
        }
    }
    if let Some(cookie) = cookie_value(headers, auth::SESSION_COOKIE) {
        if let Ok(Some(p)) = auth::lookup_session(&st.db, &cookie).await {
            return Some(p);
        }
    }
    if peer.is_loopback()
        && config::get_bool(
            &st.db,
            "auth.trust_loopback",
            config::DEFAULT_TRUST_LOOPBACK,
        )
        .await
    {
        if let Ok(Some(p)) = auth::loopback_principal(&st.db).await {
            return Some(p);
        }
    }
    None
}

/// Middleware: reject any request that doesn't resolve to a [`Principal`],
/// otherwise stash it in the request extensions for the handler.
async fn require_auth(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    match resolve_principal(&st, &headers, peer.ip()).await {
        Some(principal) => {
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        None => unauthorized("authentication required").into_response(),
    }
}

// -- Cookie + redirect helpers ----------------------------------------------

/// Build a `Set-Cookie` value for the login session. `max_age` of 0 clears it.
fn session_cookie(value: &str, max_age: i64, secure: bool) -> String {
    let mut c = format!(
        "{}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}",
        auth::SESSION_COOKIE
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Build the `Set-Cookie` value for the short-lived OAuth state cookie.
fn state_cookie(value: &str, max_age: i64) -> String {
    format!("{OAUTH_STATE_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

/// A 303 redirect to `location`, appending each given `Set-Cookie` header.
fn redirect_with_cookies(location: &str, cookies: &[String]) -> Response {
    let mut resp = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .body(axum::body::Body::empty())
        .expect("static redirect response is well-formed");
    let h = resp.headers_mut();
    for c in cookies {
        if let Ok(v) = header::HeaderValue::from_str(c) {
            h.append(header::SET_COOKIE, v);
        }
    }
    resp
}

/// Redirect back to the SPA login screen with an error code it can render.
fn login_error_redirect(code: &str) -> Response {
    redirect_with_cookies(&format!("/login?error={code}"), &[])
}

async fn cookie_secure(st: &AppState) -> bool {
    config::get_bool(&st.db, "auth.cookie_secure", config::DEFAULT_COOKIE_SECURE).await
}

/// The externally-visible base URL, for the OAuth callback. Prefers the
/// `auth.base_url` setting; otherwise derives `{proto}://{host}` from the request
/// (honouring `X-Forwarded-Proto` from a TLS-terminating proxy).
async fn external_base(st: &AppState, headers: &HeaderMap) -> Option<String> {
    let configured = config::get(&st.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !configured.is_empty() {
        return Some(configured);
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Some(format!("{proto}://{host}"))
}

// -- Identity ----------------------------------------------------------------

async fn auth_methods(st: &AppState) -> AuthMethods {
    AuthMethods {
        password: true,
        github: auth::github_oauth(&st.db).await.is_some(),
    }
}

/// `GET /api/auth/me` — who the caller is + which sign-in methods to offer.
/// Public: an unauthenticated caller gets `authenticated: false`, not a 401.
async fn auth_me(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<MeView> {
    let principal = resolve_principal(&st, &headers, peer.ip()).await;
    let methods = auth_methods(&st).await;
    Json(match principal {
        Some(p) => MeView {
            authenticated: true,
            username: Some(p.username),
            github_login: p.github_login,
            via: Some(p.via.as_str().to_string()),
            methods,
        },
        None => MeView {
            authenticated: false,
            username: None,
            github_login: None,
            via: None,
            methods,
        },
    })
}

/// `POST /api/auth/login` — username/password. Sets the session cookie.
async fn auth_login(State(st): State<AppState>, Json(body): Json<LoginReq>) -> ApiResult<Response> {
    let principal = auth::verify_login(&st.db, body.username.trim(), &body.password)
        .await?
        .ok_or_else(|| unauthorized("invalid username or password"))?;
    let cookie = auth::create_session(&st.db, &principal.username).await?;
    let set = session_cookie(&cookie, SESSION_MAX_AGE, cookie_secure(&st).await);
    Ok((
        [(header::SET_COOKIE, set)],
        Json(json!({ "username": principal.username })),
    )
        .into_response())
}

/// `POST /api/auth/logout` — drop the session and clear the cookie.
async fn auth_logout(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(cookie) = cookie_value(&headers, auth::SESSION_COOKIE) {
        auth::delete_session(&st.db, &cookie).await.ok();
    }
    let clear = session_cookie("", 0, cookie_secure(&st).await);
    Ok(([(header::SET_COOKIE, clear)], Json(json!({ "ok": true }))).into_response())
}

// -- GitHub OAuth ------------------------------------------------------------

/// `GET /api/auth/github/login` — begin the OAuth dance.
async fn github_login(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let cfg = auth::github_oauth(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("GitHub sign-in is not configured"))?;
    let base = external_base(&st, &headers).await.ok_or_else(|| {
        AppError::bad_request("cannot determine the callback URL (no Host header)")
    })?;
    let redirect_uri = format!("{base}{GITHUB_CALLBACK_PATH}");
    let state = auth::random_state();
    let url = auth::authorize_url(&cfg, &state, &redirect_uri);
    Ok(redirect_with_cookies(&url, &[state_cookie(&state, 600)]))
}

#[derive(Debug, Deserialize)]
struct GithubCallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// `GET /api/auth/github/callback` — finish the dance: verify state, exchange the
/// code, check the GitHub login against the allowlist, open a session.
async fn github_callback(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<GithubCallbackQuery>,
) -> ApiResult<Response> {
    let cfg = auth::github_oauth(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("GitHub sign-in is not configured"))?;
    // CSRF: the returned state must match the cookie we set at /login.
    let expected = cookie_value(&headers, OAUTH_STATE_COOKIE);
    if expected.is_none() || q.state.is_none() || expected != q.state {
        return Ok(login_error_redirect("state-mismatch"));
    }
    let Some(code) = q.code.filter(|c| !c.is_empty()) else {
        return Ok(login_error_redirect("missing-code"));
    };
    let base = external_base(&st, &headers)
        .await
        .ok_or_else(|| AppError::bad_request("cannot determine the callback URL"))?;
    let redirect_uri = format!("{base}{GITHUB_CALLBACK_PATH}");
    let token = auth::exchange_code(&cfg, &code, &redirect_uri).await?;
    let login = auth::fetch_github_login(&token).await?;
    let Some(user) = auth::user_by_github(&st.db, &login).await? else {
        // Authenticated with GitHub, but not on the allowlist.
        return Ok(login_error_redirect("not-approved"));
    };
    let cookie = auth::create_session(&st.db, &user.username).await?;
    Ok(redirect_with_cookies(
        "/",
        &[
            session_cookie(&cookie, SESSION_MAX_AGE, cookie_secure(&st).await),
            state_cookie("", 0),
        ],
    ))
}

// -- API tokens --------------------------------------------------------------

fn token_view(info: auth::TokenInfo) -> TokenView {
    TokenView {
        id: info.id,
        name: info.name,
        prefix: info.prefix,
        created_at: info.created_at,
        last_used_at: info.last_used_at,
        expires_at: info.expires_at,
    }
}

/// `GET /api/auth/tokens` — the user-managed API tokens.
async fn list_tokens(State(st): State<AppState>) -> ApiResult<Json<Vec<TokenView>>> {
    let tokens = auth::list_tokens(&st.db).await?;
    Ok(Json(tokens.into_iter().map(token_view).collect()))
}

/// `POST /api/auth/tokens` — mint a token, returning the plaintext once.
async fn create_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<CreateTokenReq>,
) -> ApiResult<Json<CreatedTokenView>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("a token name is required"));
    }
    let (token, info) =
        auth::create_token(&st.db, &principal.username, name, body.expires_in_days).await?;
    Ok(Json(CreatedTokenView {
        token,
        info: token_view(info),
    }))
}

/// `DELETE /api/auth/tokens/{id}` — revoke a token.
async fn revoke_token(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if auth::revoke_token(&st.db, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("token"))
    }
}

// -- Account + users ---------------------------------------------------------

/// `POST /api/auth/password` — set/change the caller's own password.
async fn set_own_password(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<SetPasswordReq>,
) -> ApiResult<StatusCode> {
    if body.new_password.len() < 8 {
        return Err(AppError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    auth::set_password(&st.db, &principal.username, Some(&body.new_password)).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn user_view(u: auth::User) -> UserView {
    let has_password = u.has_password();
    UserView {
        username: u.username,
        github_login: u.github_login,
        has_password,
        created_at: u.created_at,
    }
}

/// `GET /api/auth/users` — the approved-operator allowlist.
async fn list_users(State(st): State<AppState>) -> ApiResult<Json<Vec<UserView>>> {
    let users = auth::list_users(&st.db).await?;
    Ok(Json(users.into_iter().map(user_view).collect()))
}

/// `POST /api/auth/users` — approve a new operator.
async fn add_user(
    State(st): State<AppState>,
    Json(body): Json<AddUserReq>,
) -> ApiResult<Json<UserView>> {
    let username = body.username.trim();
    if username.is_empty() {
        return Err(AppError::bad_request("a username is required"));
    }
    let github = body
        .github_login
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = body.password.as_deref().filter(|s| !s.is_empty());
    if github.is_none() && password.is_none() {
        return Err(AppError::bad_request(
            "set a GitHub login or a password so the user can sign in",
        ));
    }
    if let Some(p) = password {
        if p.len() < 8 {
            return Err(AppError::bad_request(
                "password must be at least 8 characters",
            ));
        }
    }
    auth::add_user(&st.db, username, github, password)
        .await
        .map_err(|e| AppError::bad_request(format!("could not add user: {e}")))?;
    let user = auth::get_user(&st.db, username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(Json(user_view(user)))
}

/// `DELETE /api/auth/users/{username}` — remove an approved operator.
async fn remove_user(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    if username == principal.username {
        return Err(AppError::bad_request("you cannot remove yourself"));
    }
    match auth::remove_user(&st.db, &username).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(AppError::not_found("user")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

// -- GitHub OAuth app config -------------------------------------------------

async fn github_config_view(st: &AppState) -> ApiResult<GithubConfigView> {
    let client_id = config::get(&st.db, auth::GH_CLIENT_ID_KEY)
        .await
        .unwrap_or_default();
    Ok(GithubConfigView {
        configured: auth::github_oauth(&st.db).await.is_some(),
        client_id,
        callback_path: GITHUB_CALLBACK_PATH.to_string(),
    })
}

/// `GET /api/auth/github/config` — the GitHub sign-in setup (secret withheld).
async fn get_github_config(State(st): State<AppState>) -> ApiResult<Json<GithubConfigView>> {
    Ok(Json(github_config_view(&st).await?))
}

/// `PUT /api/auth/github/config` — set the OAuth app id (and, optionally, secret).
async fn put_github_config(
    State(st): State<AppState>,
    Json(body): Json<SetGithubConfigReq>,
) -> ApiResult<Json<GithubConfigView>> {
    let mut changes: Vec<config::Change> = vec![(
        auth::GH_CLIENT_ID_KEY.to_string(),
        Some(body.client_id.trim().to_string()),
    )];
    // The secret is write-only: a value sets it, an empty string clears it, and
    // omitting the field leaves the stored secret untouched.
    if let Some(secret) = body.client_secret {
        let secret = secret.trim().to_string();
        changes.push((
            auth::GH_CLIENT_SECRET_KEY.to_string(),
            (!secret.is_empty()).then_some(secret),
        ));
    }
    config::apply(&st.db, &changes).await?;
    Ok(Json(github_config_view(&st).await?))
}

// ---------------------------------------------------------------------------
// Overlookers — the operator + authoring surface (server-owned state)
// ---------------------------------------------------------------------------

/// Build an [`OverlookerView`] for an overlooker, joining the most recent
/// round's outcome from the run history.
async fn overlooker_view(db: &Db, o: &Overlooker) -> ApiResult<OverlookerView> {
    let last_outcome = ov::recent_runs(db, &o.id, 1)
        .await?
        .into_iter()
        .next()
        .map(|r| r.outcome);
    Ok(OverlookerView::from_parts(o, last_outcome))
}

#[derive(Debug, Deserialize)]
struct RunsQuery {
    /// How many recent rounds to return; defaults to 50.
    limit: Option<i64>,
}

/// Reject a capability set that isn't a subset of the known ladder, naming the
/// offender. Returns the cleaned set on success.
fn validate_capabilities(caps: &[String]) -> ApiResult<()> {
    for c in caps {
        if !ov::CAPABILITIES.contains(&c.as_str()) {
            return Err(AppError::bad_request(format!(
                "unknown capability '{c}' — expected a subset of {}",
                ov::CAPABILITIES.join(", ")
            )));
        }
    }
    Ok(())
}

/// A program reference must be a known `builtin:<name>` program or an absolute
/// path (a file under `~/.weaver/overlookers/`). An unknown builtin is rejected
/// here, naming the registry, rather than erroring every round; a bare relative
/// path is rejected so the engine never resolves it against an ambiguous cwd.
fn validate_program(program: &str) -> ApiResult<()> {
    if program.starts_with("builtin:") {
        if crate::builtins::find(program).is_none() {
            let known = crate::builtins::BUILTINS
                .iter()
                .map(|b| b.program())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AppError::bad_request(format!(
                "unknown builtin program '{program}' — expected one of {known}"
            )));
        }
        return Ok(());
    }
    if !PathBuf::from(program).is_absolute() {
        return Err(AppError::bad_request(format!(
            "invalid program '{program}' — expected 'builtin:<name>' or an absolute path"
        )));
    }
    Ok(())
}

/// `GET /api/overlookers/programs` — the builtin program registry: what the
/// create form offers and the panel's read-only script viewer renders.
async fn list_programs() -> Json<Vec<ProgramView>> {
    Json(crate::builtins::BUILTINS.iter().map(|b| b.view()).collect())
}

/// Resolve an overlooker (by id or name) or 404.
async fn require_overlooker(db: &Db, key: &str) -> ApiResult<Overlooker> {
    ov::resolve(db, key)
        .await?
        .ok_or_else(|| AppError::not_found("overlooker"))
}

async fn list_overlookers(State(st): State<AppState>) -> ApiResult<Json<Vec<OverlookerView>>> {
    let mut out = Vec::new();
    for o in ov::list(&st.db).await? {
        out.push(overlooker_view(&st.db, &o).await?);
    }
    Ok(Json(out))
}

async fn create_overlooker(
    State(st): State<AppState>,
    Json(req): Json<CreateOverlookerReq>,
) -> ApiResult<Json<OverlookerView>> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name must not be empty"));
    }
    if ov::get_by_name(&st.db, &name).await?.is_some() {
        return Err(AppError::conflict(format!(
            "an overlooker named '{name}' already exists"
        )));
    }

    let defaults = ov::NewOverlooker::default();
    let program = req.program.unwrap_or(defaults.program);
    validate_program(&program)?;
    let capabilities = req.capabilities.unwrap_or(defaults.capabilities);
    validate_capabilities(&capabilities)?;
    let params = json_text(req.params, &defaults.params);

    // The script declares what wakes it: evaluate its subscription manifest
    // (register mode) unless the caller pinned an explicit trigger.
    let trigger_spec = match req.trigger {
        Some(t) => t.to_string(),
        None => {
            let params_value = serde_json::from_str(&params).unwrap_or_else(|_| json!({}));
            let fallback = program_default_trigger(&program).unwrap_or(defaults.trigger_spec);
            reconcile_trigger(&st, &program, &params_value, &fallback).await
        }
    };

    let new = ov::NewOverlooker {
        name,
        trigger_spec,
        scope: json_text(req.scope, &defaults.scope),
        program,
        params,
        capabilities,
        model: req.model.unwrap_or(defaults.model),
        effort: req.effort.unwrap_or(defaults.effort),
        cooldown_secs: req.cooldown_secs.unwrap_or(defaults.cooldown_secs),
        enabled: req.enabled.unwrap_or(defaults.enabled),
    };
    let o = ov::create(&st.db, &new).await?;
    Ok(Json(overlooker_view(&st.db, &o).await?))
}

/// The program's default trigger (a builtin's suggested manifest), used as the
/// fallback when register-mode manifest evaluation declares none or fails.
fn program_default_trigger(program: &str) -> Option<String> {
    crate::builtins::find(program).map(|b| b.default_trigger.to_string())
}

/// Resolve a program's stored trigger from its register-mode manifest, falling
/// back to `fallback` when the script declares no manifest or evaluation fails
/// (a missing interpreter, a syntax error) — best-effort, never an error that
/// blocks creating the watch.
async fn reconcile_trigger(st: &AppState, program: &str, params: &Value, fallback: &str) -> String {
    match ov_engine::evaluate_manifest(st, program, params).await {
        Ok(Some(t)) => serde_json::to_string(&t).unwrap_or_else(|_| fallback.to_string()),
        Ok(None) => fallback.to_string(),
        Err(e) => {
            tracing::debug!(program, error = %e, "manifest evaluation failed; using default trigger");
            fallback.to_string()
        }
    }
}

async fn get_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<OverlookerView>> {
    let o = require_overlooker(&st.db, &key).await?;
    Ok(Json(overlooker_view(&st.db, &o).await?))
}

async fn patch_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchOverlookerReq>,
) -> ApiResult<Json<OverlookerView>> {
    let o = require_overlooker(&st.db, &key).await?;

    if let Some(program) = &req.program {
        validate_program(program)?;
    }
    if let Some(caps) = &req.capabilities {
        validate_capabilities(caps)?;
    }
    if let Some(enabled) = req.enabled {
        ov::set_enabled(&st.db, &o.id, enabled).await?;
    }
    // An explicit trigger wins; otherwise, when the program changes, re-evaluate
    // the new script's manifest (with the effective params) so subscriptions
    // follow the script — the same reconcile create does.
    let trigger_spec = match &req.trigger {
        Some(t) => Some(t.to_string()),
        None => match &req.program {
            Some(program) => {
                let params = req.params.clone().unwrap_or_else(|| o.params());
                let fallback =
                    program_default_trigger(program).unwrap_or_else(|| o.trigger_spec.clone());
                Some(reconcile_trigger(&st, program, &params, &fallback).await)
            }
            None => None,
        },
    };
    let patch = ov::OverlookerUpdate {
        trigger_spec,
        scope: req.scope.map(|v| v.to_string()),
        program: req.program,
        params: req.params.map(|v| v.to_string()),
        capabilities: req.capabilities,
        model: req.model,
        effort: req.effort,
        cooldown_secs: req.cooldown_secs,
    };
    if !patch.is_empty() {
        ov::update(&st.db, &o.id, &patch).await?;
    }
    let o = require_overlooker(&st.db, &o.id).await?;
    Ok(Json(overlooker_view(&st.db, &o).await?))
}

async fn delete_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let o = require_overlooker(&st.db, &key).await?;
    ov::delete(&st.db, &o.id).await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Fire a round now, in the daemon (the single terminal owner), and report its
/// outcome. `dry_run` stubs every mutating action — the iteration primitive,
/// safe to repeat. Re-reads the closed run row to surface outcome + summary.
async fn run_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<RunOverlookerReq>,
) -> ApiResult<Json<Value>> {
    let o = require_overlooker(&st.db, &key).await?;
    let reason = if req.dry_run { "run (dry)" } else { "run" };
    let run_id = ov_engine::fire_now(&st, &o.id, req.dry_run, reason).await?;
    let run = ov::recent_runs(&st.db, &o.id, 50)
        .await?
        .into_iter()
        .find(|r| r.id == run_id);
    let (outcome, summary) = run
        .map(|r| (r.outcome, r.summary))
        .unwrap_or_else(|| (String::new(), String::new()));
    Ok(Json(json!({
        "run_id": run_id,
        "outcome": outcome,
        "summary": summary,
    })))
}

/// Run a one-shot headless agent and return `{output}` — the judgement
/// primitive overlooker programs call. The daemon owns the agent command
/// (`WEAVER_OVERLOOKER_AGENT_CMD`, default `claude -p`) and the timeout
/// budget. Best-effort by contract: an absent or failing agent returns
/// `{output: null}` rather than an error, so callers degrade to their
/// deterministic fallback.
async fn agent_oneshot(
    State(st): State<AppState>,
    Json(req): Json<AgentOneshotReq>,
) -> ApiResult<Json<Value>> {
    if req.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt must be non-empty"));
    }
    let budget = ov_engine::get_int(&st.db, "overlooker.default_timeout_secs", 600)
        .await
        .max(1) as u64;
    let output = agent::run_oneshot(
        &req.prompt,
        &req.model,
        &req.effort,
        std::time::Duration::from_secs(budget),
    )
    .await;
    Ok(Json(json!({ "output": output })))
}

async fn overlooker_runs(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Vec<OverlookerRunView>>> {
    let o = require_overlooker(&st.db, &key).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let runs = ov::recent_runs(&st.db, &o.id, limit).await?;
    Ok(Json(
        runs.into_iter().map(OverlookerRunView::from).collect(),
    ))
}

/// Serialize an optional structured-JSON field into the text column the model
/// stores, falling back to the model default when absent.
fn json_text(value: Option<Value>, default: &str) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| default.to_string())
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
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db.clone()),
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
        assert!(note.contains("weaver status"));
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

    #[test]
    fn is_immutable_asset_matches_rspack_content_hashed_files() {
        assert!(is_immutable_asset("/app.a1b2c3d4.js"));
        assert!(is_immutable_asset("/chunk.00ff1234.js"));
        assert!(is_immutable_asset("/styles.deadbeef.css"));
    }

    #[test]
    fn is_immutable_asset_rejects_non_hashed_paths() {
        assert!(!is_immutable_asset("/index.html"));
        assert!(!is_immutable_asset("/app.js"));
        assert!(!is_immutable_asset("/favicon.ico"));
        // Hash segment must be exactly 8 hex chars.
        assert!(!is_immutable_asset("/app.abc.js"));
        assert!(!is_immutable_asset("/app.abc123def.js")); // 9 chars
    }

    #[test]
    fn is_ide_proxy_path_matches_proxy_and_subpaths_in_both_forms() {
        // Nest-stripped form (what the middleware actually sees) …
        assert!(is_ide_proxy_path("/sessions/abc/ide"));
        assert!(is_ide_proxy_path("/sessions/abc/ide/"));
        assert!(is_ide_proxy_path("/sessions/abc/ide/static/out/main.js"));
        // … and the `/api`-prefixed form, in case the layer ever moves outward.
        assert!(is_ide_proxy_path("/api/sessions/abc/ide"));
        assert!(is_ide_proxy_path(
            "/api/sessions/abc/ide/static/out/main.js"
        ));
    }

    #[test]
    fn is_ide_proxy_path_rejects_siblings_and_non_ide_routes() {
        // `ide-info` is JSON that *should* be ETagged — not the proxy.
        assert!(!is_ide_proxy_path("/sessions/abc/ide-info"));
        assert!(!is_ide_proxy_path("/api/sessions/abc/ide-info"));
        assert!(!is_ide_proxy_path("/sessions/abc/log"));
        assert!(!is_ide_proxy_path("/sessions/abc"));
        assert!(!is_ide_proxy_path("/sessions"));
        assert!(!is_ide_proxy_path("/repos/issues"));
    }
}
