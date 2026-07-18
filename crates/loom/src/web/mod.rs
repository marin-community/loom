//! axum REST API + SSE. The Vue SPA is the primary consumer.
//!
//! Endpoint layout (post phase-4 rename):
//!
//! * `/api/sessions` — list + create active sessions (each session is one
//!   terminal + one agent attached to a branch).
//! * `/api/sessions/{id}` — GET / PATCH / DELETE a single session, plus the
//!   action subroutes `/archive`, `/adopt`, `/recover` (rebuild the worktree of
//!   an archived session and resume its agent), `/tags/{key}` (PUT to set a tag,
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
//! watch's `triage` — are **tags**: well-known keys under `tags`, set
//! through `PUT /api/sessions/{id}/tags/{key}` and cleared through `DELETE`.
//! Absence is the calm state; there is no stored `ok` tag.

mod agents;
mod artifacts;
mod auth;
mod branches;
mod discussion;
mod env;
mod issues;
mod logview;
mod repo_env;
mod repos;
mod scratch;
mod sessions;
mod settings;
mod watches;

use agents::*;
use artifacts::*;
use auth::*;
use branches::*;
use discussion::*;
use env::*;
use issues::*;
use logview::*;
use repo_env::*;
use repos::*;
use scratch::*;
use sessions::*;
use settings::*;
use watches::*;

// Re-exported so the rest of the crate (server.rs, github.rs, watch.rs)
// can keep calling these as `crate::web::{archive, adopt, create_warm_session}`
// — they're session lifecycle operations, but not routed through this file's
// `router()`, so the glob imports above don't cover them.
pub(crate) use sessions::{adopt, archive, create_warm_session};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::Instrument;

use crate::db::Db;
use crate::events::EventBus;
use crate::github;
use crate::session::{self as session_mod, Session};
use weaver_api::{BranchView, SessionView};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
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
    /// Extra keys merged into the body alongside `error` (top-level, not
    /// nested under `details`) — for callers whose wire contract is a flat
    /// object, e.g. the artifact write-conflict `{ "error", "latest" }`.
    fields: Option<Value>,
    /// For an internal error built from an `anyhow::Error`: the full cause chain
    /// (and backtrace, when `RUST_BACKTRACE` is set), logged server-side so an
    /// operator sees *why* — the client still gets only the concise `message`.
    source_chain: Option<String>,
}

impl AppError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
            fields: None,
            source_chain: None,
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
    /// Merge `fields` (must be a JSON object) into the response body
    /// top-level, alongside `error`.
    fn with_fields(mut self, fields: Value) -> Self {
        self.fields = Some(fields);
        self
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    #[cfg(test)]
    fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            // Log the full cause chain (and backtrace when captured), not just the
            // top-level message, so the log says *why* the request 500'd.
            tracing::error!(
                status = %self.status.as_u16(),
                error = %self.source_chain.as_deref().unwrap_or(&self.message),
                "request failed"
            );
        } else {
            tracing::warn!(status = %self.status.as_u16(), message = %self.message, "request rejected");
        }
        let mut body = json!({ "error": self.message });
        if let Some(details) = self.details {
            body["details"] = details;
        }
        if let Some(Value::Object(fields)) = self.fields {
            if let Value::Object(map) = &mut body {
                map.extend(fields);
            }
        }
        (self.status, Json(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        let err = err.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
            details: None,
            fields: None,
            // `{err:?}` renders anyhow's full cause chain plus the backtrace when
            // one was captured (`RUST_BACKTRACE=1`); `to_string()` above is just
            // the top-level message the client sees.
            source_chain: Some(format!("{err:?}")),
        }
    }
}

pub(crate) type ApiResult<T> = Result<T, AppError>;

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
pub(crate) async fn branch_view(db: &Db, branch: &Branch) -> ApiResult<BranchView> {
    // Every tag (the agent's `attention`, a watch's `triage`, any free-form
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
pub(crate) async fn session_view(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> ApiResult<SessionView> {
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
        park: session.park.clone(),
        sort_order: session.sort_order,
        branch: bv,
    })
}

/// Resolve a session key (session id, branch id, branch name, or `repo:branch`)
/// to `(Session, Branch)`. The session must exist and be active; clients hitting
/// a branch with no live session get a 404.
pub(crate) async fn require_session(db: &Db, key: &str) -> ApiResult<(Session, Branch)> {
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

/// The dashboard URL for a session — the page a person opens to watch it.
/// `base` is an origin (with or without a trailing slash); pair it with
/// [`auth::public_base`] to build a link that resolves off-box.
pub(crate) fn session_url(base: &str, session_id: &str) -> String {
    format!("{}/s/{session_id}", base.trim_end_matches('/'))
}

/// The dashboard deep-link for an artifact — the page a person opens to read it
/// (`/s/:id/artifacts/:name` in the SPA router). `key` is any session key (the
/// `$WEAVER_BRANCH` an agent carries resolves fine); pair `base` with
/// [`auth::public_base`] so the link resolves off-box.
pub(crate) fn artifact_url(base: &str, key: &str, name: &str) -> String {
    format!("{}/s/{key}/artifacts/{name}", base.trim_end_matches('/'))
}

pub(crate) async fn require_branch(db: &Db, key: &str) -> ApiResult<Branch> {
    if let Some(branch) = branch_mod::resolve_key(db, key).await? {
        return Ok(branch);
    }
    if let Some((_, branch)) = session_mod::with_branch(db, key).await? {
        return Ok(branch);
    }
    Err(AppError::not_found("branch"))
}

/// The author of a mutation: the trimmed `by`, or `manual` when absent or
/// all-whitespace (an empty author never reaches the audit trail).
pub(crate) fn author_or_manual(by: Option<&str>) -> String {
    by.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("manual")
        .to_string()
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
async fn api_etag_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
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
async fn static_cache_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
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

/// Outermost middleware: open a per-request tracing span carrying the method and
/// path, so *every* log line emitted while the request is handled — an auth
/// rejection, a validation `warn`, an internal `error` — is tagged with which
/// request produced it. Without it a bare `authentication required status=401`
/// tells an operator nothing about *what* was being accessed. The span's fields
/// are folded into each line by [`crate::logs::CaptureLayer`].
async fn request_context_span(request: Request<axum::body::Body>, next: Next) -> Response {
    let span = tracing::info_span!(
        "http",
        method = %request.method(),
        path = %request.uri().path(),
    );
    next.run(request).instrument(span).await
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
        // The session's own dashboard URL — the link an agent hands a human.
        .route("/sessions/{id}/url", get(session_url_route))
        .route("/sessions/{id}/archive", post(archive_session))
        .route("/sessions/{id}/adopt", post(adopt_session))
        .route("/sessions/{id}/recover", post(recover_session))
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
            "/sessions/{id}/artifacts/{name}/threads",
            get(list_threads).post(create_thread),
        )
        .route(
            "/sessions/{id}/artifacts/{name}/threads/{tid}/comments",
            post(add_comment),
        )
        .route(
            "/sessions/{id}/artifacts/{name}/threads/{tid}/resolve",
            post(resolve_thread),
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
        // Command routes: each is a multi-write + event sequence the `weaver`
        // CLI needs done atomically server-side, not composed client-side out
        // of generic PATCH calls (which would miss the event or race a
        // partial write). No live session required — `weaver` runs as an
        // HTTP-only client of loom and these are its primary write path.
        .route("/branches/{id}/status", post(set_branch_status))
        .route("/branches/{id}/events", post(create_branch_event))
        .route(
            "/branches/{id}/tags/{key}",
            axum::routing::put(set_branch_tag).delete(clear_branch_tag),
        )
        .route("/branches/{id}/artifacts", get(list_branch_artifacts))
        .route(
            "/branches/{id}/artifacts/{name}",
            get(get_branch_artifact)
                .put(write_branch_artifact)
                .delete(delete_branch_artifact),
        )
        .route(
            "/branches/{id}/artifacts/{name}/url",
            get(branch_artifact_url_route),
        )
        .route(
            "/branches/{id}/artifacts/{name}/threads",
            get(list_branch_threads).post(create_branch_thread),
        )
        .route(
            "/branches/{id}/artifacts/{name}/threads/{tid}/comments",
            post(add_branch_thread_comment),
        )
        .route(
            "/branches/{id}/artifacts/{name}/threads/{tid}/resolve",
            post(resolve_branch_thread),
        )
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
        // Operator-defined custom agents (create + edit/remove by name). The
        // static `/custom` segment is registered before the `{name}` capture.
        .route("/agents/custom", post(create_custom_agent))
        .route(
            "/agents/custom/{name}",
            axum::routing::put(update_custom_agent).delete(delete_custom_agent),
        )
        // The managed repo store + clone allowlist (register/list).
        .route("/repos", get(list_repos).post(register_repo))
        .route("/repos/recent", get(recent_repos))
        .route("/repos/branches", get(repo_branches))
        .route(
            "/repos/issues",
            get(list_repo_issues).post(create_repo_issue),
        )
        // Per-repo environment variables (write-only values), layered into a
        // session's terminal above the global agent_env.
        .route("/repos/env", get(get_repo_env))
        .route(
            "/repos/env/{name}",
            axum::routing::put(put_repo_env).delete(delete_repo_env),
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
        // Server logs + background tasks (Settings → Debug) — snapshot + live SSE
        // tail + build status + the detached trigger-task list. Operator-only:
        // server logs can carry tokens injected into agents.
        .route("/logs", get(logs_snapshot))
        .route("/logs/stream", get(logs_stream))
        .route("/status", get(server_status))
        .route("/tasks", get(tasks_snapshot))
        // Watches — periodic / triggered watch programs over the fleet.
        .route("/watches", get(list_watches).post(create_watch))
        // The static segment wins over the `{id}` capture below, so a program
        // named "programs" can't shadow this listing.
        .route("/watches/programs", get(list_programs))
        .route(
            "/watches/{id}",
            get(get_watch).patch(patch_watch).delete(delete_watch),
        )
        .route("/watches/{id}/run", post(run_watch))
        .route("/watches/{id}/runs", get(watch_runs))
        // The one-shot headless agent — the judgement primitive watch
        // programs (and any script) call through the daemon.
        .route("/agent/oneshot", post(agent_oneshot))
        // Authentication management: API tokens, the caller's password, the
        // approved-user allowlist, and the GitHub OAuth app config.
        .route("/auth/tokens", get(list_tokens).post(create_token))
        .route("/auth/tokens/{id}", delete(revoke_token))
        .route("/auth/password", post(set_own_password))
        // The caller's own GitHub token (a fine-grained PAT), injected as
        // GH_TOKEN into the sessions they launch so their agents act as them.
        .route(
            "/auth/github-token",
            get(get_github_token)
                .put(set_github_token)
                .delete(delete_github_token),
        )
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
        // Outermost, so it wraps auth and every other layer: tag each request's log
        // lines with its method + path (see `request_context_span`).
        .layer(axum::middleware::from_fn(request_context_span))
}

#[cfg(test)]
mod tests {
    use super::*;

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
