//! axum REST API + SSE. The Vue SPA is the primary consumer.
//!
//! Endpoint layout (post phase-4 rename):
//!
//! * `/api/sessions` — list + create active sessions (each session is one
//!   terminal + one agent attached to a branch).
//! * `/api/sessions/{id}` — GET / PATCH / DELETE a single session, plus the
//!   action subroutes `/archive`, `/adopt`, `/recover` (rebuild an archived
//!   session or restart a failed live ACP runtime), `/tags` (PUT to atomically
//!   replace one author's tag set), `/tags/{key}` (PUT to set a tag, DELETE to
//!   clear it), `/log`, `/events`, and `/terminal` (a WebSocket bridged to the
//!   session's terminal via a PTY — see `crate::terminal`).
//!   Interacting with the agent (keystrokes, keys, TUIs) happens entirely over
//!   `/terminal`.
//! * `/api/branches` — list every tracked branch (with or without an active
//!   session). `/api/branches/{id}` — GET / PATCH (goal / title / description).
//! * `/api/branches/{id}/issues` — list / POST issues for a branch.
//! * `/api/issues/{id}` — GET / PATCH / DELETE an issue by id.
//! * `/api/issues/actions` — validate and atomically mutate a set of issues.
//! * `/api/health` + `/api/health/live` are process-level liveness;
//!   `/api/ready` checks the database and migration streams; `/metrics` exposes
//!   bounded-label OpenMetrics; `/api/diagnostics` is the admin inventory.
//! * `/api/repos/recent`, `/api/repos/branches`, `/api/settings` — unchanged.
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
mod automation;
mod branches;
mod changes;
mod channels;
mod deployment;
mod diagnostics;
mod discussion;
mod env;
mod eventmux;
mod issues;
mod launches;
mod logview;
mod mcps;
mod profiles;
mod repo_env;
mod repos;
mod restricted_github;
mod reviews;
mod scratch;
mod self_context;
mod session_layout;
pub(crate) mod sessions;
mod settings;
mod watches;

use agents::*;
use artifacts::*;
use auth::*;
use automation::*;
use branches::*;
use changes::*;
use channels::*;
use deployment::*;
use diagnostics::*;
use discussion::*;
use env::*;
use eventmux::*;
use issues::*;
use launches::*;
use logview::*;
use mcps::*;
use profiles::*;
use repo_env::*;
use repos::*;
use restricted_github::*;
use reviews::*;
use scratch::*;
use self_context::*;
use session_layout::*;
use sessions::*;
use settings::*;
use watches::*;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use axum::{
    body::HttpBody as _,
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
use crate::github;
use crate::session::{self as session_mod, Session};
use crate::AppState;
use weaver_api::{
    BranchSummaryView, BranchView, McpPolicySnapshot, SessionMcpPolicyView, SessionSummaryView,
    SessionView,
};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::tags;

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
    fn internal(message: impl Into<String>, error: impl Into<anyhow::Error>) -> Self {
        let error = error.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            details: None,
            fields: None,
            source_chain: Some(format!("{error:?}")),
        }
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
    fn is_not_found(&self) -> bool {
        self.status == StatusCode::NOT_FOUND
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
        // A lifecycle transition that refused for a reason the caller could have
        // avoided carries the status it means; anything else is a real 500.
        let status = match err.downcast_ref::<crate::lifecycle::Refusal>() {
            Some(crate::lifecycle::Refusal::Conflict(_)) => StatusCode::CONFLICT,
            Some(crate::lifecycle::Refusal::Invalid(_)) => StatusCode::BAD_REQUEST,
            None => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
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

fn provision_error(error: crate::provision::ProvisionError) -> AppError {
    use crate::provision::ProvisionError::*;
    let (status, message, preview, session_id) = match error {
        Invalid(message, preview) => (
            StatusCode::BAD_REQUEST,
            message,
            preview.map(|preview| *preview),
            None,
        ),
        Forbidden(message) => (StatusCode::FORBIDDEN, message, None, None),
        NotFound(message) => (StatusCode::NOT_FOUND, message, None, None),
        Conflict(message, preview) => (
            StatusCode::CONFLICT,
            message,
            preview.map(|preview| *preview),
            None,
        ),
        CredentialRequired(message) => (StatusCode::PRECONDITION_REQUIRED, message, None, None),
        ExternalFailure(message, session_id) => {
            (StatusCode::BAD_GATEWAY, message, None, session_id)
        }
        Internal(error) => return error.into(),
    };
    let mut fields = serde_json::Map::new();
    if let Some(preview) = preview {
        fields.insert("preview".to_string(), json!(preview));
    }
    if let Some(session_id) = session_id {
        fields.insert("session_id".to_string(), json!(session_id));
    }
    let mapped = AppError::new(status, message);
    if fields.is_empty() {
        mapped
    } else {
        mapped.with_fields(Value::Object(fields))
    }
}

pub(crate) type ApiResult<T> = Result<T, AppError>;

/// JSON base64 expands a valid 50 MiB Scratch batch by roughly one third.
/// Keep this route envelope above that encoded payload while retaining the
/// smaller protected-router default for unrelated endpoints.
const MAX_SESSION_CREATE_BODY_BYTES: usize = 72 * 1024 * 1024;

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
    let github_pr = github::get_mapping(db, &branch.id).await.ok().flatten();
    Ok(BranchView::from_parts(
        branch, &tags, open, github, github_pr,
    ))
}

/// Build a [`SessionView`] for a session + its branch.
pub(crate) async fn session_view(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> ApiResult<SessionView> {
    let bv = branch_view(db, branch).await?;
    let github_issue = if let Some(id) = session.tracking_issue_id {
        weaver_core::issue::get(db, id).await?.and_then(|issue| {
            match (issue.github_repo, issue.github_issue) {
                (Some(repo), Some(number)) => Some(weaver_api::GithubIssueRef { repo, number }),
                _ => None,
            }
        })
    } else {
        None
    };
    // The latest usage block is a cheap indexed query; `None` for a terminal
    // session (or an ACP session before the agent reports usage).
    let usage = if session.protocol == "acp" {
        crate::chat::latest_usage(db, &session.id).await?
    } else {
        None
    };
    let mcp_policy = serde_json::from_str::<McpPolicySnapshot>(&session.policy_mcp_access)
        .map(|snapshot| SessionMcpPolicyView::from(&snapshot))
        .map_err(|error| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("invalid session MCP policy snapshot: {error}"),
            )
        })?;
    let resolved_launch = if session.launch_snapshot.trim().is_empty() {
        None
    } else {
        Some(
            crate::launch::deserialize_snapshot(&session.launch_snapshot)
                .map_err(|error| {
                    AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("invalid session launch snapshot: {error}"),
                    )
                })?
                .view,
        )
    };
    let placement = crate::session_layout::placement(db, &session.id).await?;
    let legacy_park = placement.as_ref().and_then(|placement| {
        (placement.group_system_key.as_deref() == Some("later")).then_some("parked".to_string())
    });
    let legacy_sort_order = placement.as_ref().map(|placement| placement.rank as f64);
    let title_generation =
        crate::metadata_assist::title_view(db, &session.id, branch.title_provenance).await?;
    Ok(SessionView {
        id: session.id.clone(),
        status: session.status.clone(),
        transition: session_transition_view(session),
        work_dir: session.work_dir.clone(),
        term_session: session.term_session.clone(),
        agent_kind: session.agent_kind.clone(),
        model: session.model.clone(),
        effort: session.effort.clone(),
        github_repo: session.github_repo.clone(),
        github_issue,
        last_activity_at: session
            .last_activity_at
            .clone()
            .unwrap_or_else(|| branch.updated_at.clone()),
        created_at: session.created_at.clone(),
        updated_at: branch.updated_at.clone(),
        title_generation,
        parent_id: session.parent_branch_id.clone(),
        parent_session_id: session.parent_session_id.clone(),
        created_by: session.created_by.clone(),
        origin: session.origin.clone(),
        class: session.class.clone(),
        turn_count: session.turn_count,
        tracking_issue: session.tracking_issue_id,
        park: legacy_park,
        sort_order: legacy_sort_order,
        protocol: session.protocol.clone(),
        acp_session_id: session.acp_session_id.clone(),
        current_mode: session.current_mode.clone(),
        usage,
        profile: session.profile.clone(),
        profile_revision: session.profile_revision,
        profile_lifetime: session.profile_lifetime,
        policy_strict: session.policy_strict,
        mutation_revision: session.mutation_revision,
        launch_mode: session.launch_mode.clone(),
        mcp_policy,
        resolved_launch,
        placement,
        branch: bv,
    })
}

/// Build the compact fleet/search projection for a session + branch.
///
/// Unlike [`session_view`], this deliberately does not deserialize launch
/// snapshots, MCP policy, or title-generation state. Large goal text remains
/// available to server-side search through the source `Branch`, but crosses the
/// wire only when a client follows with the session detail endpoint.
pub(crate) async fn session_summary_view(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> ApiResult<SessionSummaryView> {
    let branch = branch_view(db, branch).await?;
    let github_issue = if let Some(id) = session.tracking_issue_id {
        weaver_core::issue::get(db, id).await?.and_then(|issue| {
            match (issue.github_repo, issue.github_issue) {
                (Some(repo), Some(number)) => Some(weaver_api::GithubIssueRef { repo, number }),
                _ => None,
            }
        })
    } else {
        None
    };
    let usage = if session.protocol == "acp" {
        crate::chat::latest_usage(db, &session.id).await?
    } else {
        None
    };
    let placement = crate::session_layout::placement(db, &session.id).await?;
    Ok(SessionSummaryView {
        id: session.id.clone(),
        status: session.status.clone(),
        transition: session_transition_view(session),
        github_repo: session.github_repo.clone(),
        github_issue,
        last_activity_at: session
            .last_activity_at
            .clone()
            .unwrap_or_else(|| branch.updated_at.clone()),
        created_at: session.created_at.clone(),
        parent_id: session.parent_branch_id.clone(),
        parent_session_id: session.parent_session_id.clone(),
        created_by: session.created_by.clone(),
        origin: session.origin.clone(),
        class: session.class.clone(),
        tracking_issue: session.tracking_issue_id,
        profile: session.profile.clone(),
        usage,
        placement,
        branch: BranchSummaryView::from(&branch),
    })
}

fn session_transition_view(session: &Session) -> Option<weaver_api::SessionTransitionView> {
    Some(weaver_api::SessionTransitionView {
        kind: session.lifecycle_transition.clone()?,
        step: session.lifecycle_step.clone().unwrap_or_default(),
        started_at: session
            .lifecycle_transition_started_at
            .clone()
            .unwrap_or_default(),
    })
}

/// Resolve a session key (session id, branch id, branch name, or `repo:branch`)
/// to `(Session, Branch)`. The session must exist and be active; clients hitting
/// a branch with no live session get a 404.
pub(crate) async fn require_session(db: &Db, key: &str) -> ApiResult<(Session, Branch)> {
    session_mod::resolve_key(db, key)
        .await?
        .ok_or_else(|| AppError::not_found("session"))
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
/// Largest response body buffered to compute an ETag. Anything bigger is served
/// unhashed rather than buffered — see [`api_etag_middleware`].
const ETAG_MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

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
    // An ETag is an optimization; buffering to compute one must never be able to
    // damage the response. A body whose known length is over the cap streams
    // straight through unhashed — a long agent transcript is exactly the payload
    // that outgrows this, and silently serving it as an empty 200 renders as a
    // blank conversation with no error anywhere.
    if body
        .size_hint()
        .exact()
        .is_some_and(|len| len > ETAG_MAX_BODY_BYTES)
    {
        return Response::from_parts(parts, body);
    }
    let bytes = match axum::body::to_bytes(body, ETAG_MAX_BODY_BYTES as usize).await {
        Ok(b) => b,
        // Unknown-length body that outgrew the cap. `to_bytes` has already
        // consumed it, so it cannot be forwarded — fail loudly instead of
        // handing the client a truncated success.
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "response too large to serve" })),
            )
                .into_response()
        }
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
/// - Everything else (`index.html`, icons, etc.) gets `no-store`. In particular,
///   the SPA shell must never 304 against a rapid rebuild and keep pointing at
///   an obsolete JS/CSS hash.
async fn static_cache_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;

    // API responses have their own ETag/no-cache policy. This layer is mounted
    // on the whole application router, so leave that policy intact.
    if path == "/api"
        || path.starts_with("/api/")
        || !matches!(response.status(), StatusCode::OK | StatusCode::NOT_MODIFIED)
    {
        return response;
    }

    let cache_control = if is_immutable_asset(&path) {
        "max-age=31536000, immutable"
    } else {
        "no-store, max-age=0"
    };

    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .insert(header::CACHE_CONTROL, cache_control.parse().unwrap());
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
        // `/health` remains the compatibility liveness probe. `/health/live`
        // names it explicitly; readiness checks DB + migration state.
        .route("/health", get(liveness))
        .route("/health/live", get(liveness))
        .route("/ready", get(readiness))
        .route("/health/ready", get(readiness))
        .route("/auth/me", get(auth_me))
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .route("/auth/github/login", get(github_login))
        .route("/auth/github/callback", get(github_callback))
        .route("/auth/federate", post(federate))
        // The inbound GitHub webhook. Deliberately OUTSIDE `require_auth`: it is
        // authenticated cryptographically by the HMAC signature it carries, not
        // by a loom principal. The handler is the untrusted-input boundary.
        .route("/github/webhook", post(github_webhook));

    // Everything else requires an authenticated principal — a bearer token, a
    // session cookie, or a trusted-loopback request — gated by `require_auth`.
    let protected = Router::new()
        // Every live stream the browser wants, folded onto one connection so a
        // tab spends 1 of its 6 per-origin sockets instead of 3. The per-stream
        // routes below remain the single-stream API.
        .route("/events", get(events_mux))
        // Shared Spaces → Groups → Sessions workbench organization.
        .route("/session-layout", get(get_session_layout))
        .route("/session-layout/events", get(session_layout_events))
        .route("/session-layout/spaces", post(create_session_space))
        .route(
            "/session-layout/spaces/{id}",
            axum::routing::patch(update_session_space).delete(delete_session_space),
        )
        .route("/session-layout/groups", post(create_session_group))
        .route(
            "/session-layout/groups/{id}",
            axum::routing::patch(update_session_group).delete(delete_session_group),
        )
        .route(
            "/session-layout/groups/{id}/preference",
            axum::routing::put(set_session_group_preference),
        )
        .route("/session-layout/reorder", post(reorder_session_layout))
        .route("/session-layout/moves", post(move_session_layout))
        .route("/session-layout/restores", post(restore_session_layout))
        .route(
            "/session-layout/defaults",
            axum::routing::put(set_session_placement_default),
        )
        .route(
            "/session-layout/defaults/{kind}/{value}",
            delete(delete_session_placement_default),
        )
        // Sessions
        .route(
            "/sessions",
            get(list_sessions)
                .post(create_session)
                .layer(DefaultBodyLimit::max(MAX_SESSION_CREATE_BODY_BYTES)),
        )
        .route("/self", get(get_self_context))
        .route("/session-launches/resolve", post(resolve_session_launch))
        .route("/scratch/limits", get(scratch_limits))
        .route("/sessions/summary", get(list_session_summaries))
        .route("/sessions/search", get(search_sessions))
        .route(
            "/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        // The session's own dashboard URL — the link an agent hands a human.
        .route("/sessions/{id}/url", get(session_url_route))
        .route(
            "/sessions/{id}/title/regenerate",
            post(regenerate_session_title),
        )
        .route(
            "/sessions/{id}/title-generation",
            axum::routing::put(set_session_title_generation),
        )
        .route(
            "/sessions/{id}/resumption-cue",
            get(get_resumption_cue).post(ensure_resumption_cue),
        )
        .route("/sessions/{id}/archive", post(archive_session))
        .route(
            "/sessions/{id}/restricted-github/{tool}",
            post(restricted_github_tool),
        )
        .route("/sessions/{id}/adopt", post(adopt_session))
        .route(
            "/sessions/{id}/handoff/resolve",
            post(resolve_session_handoff),
        )
        .route("/sessions/{id}/handoff", post(handoff_session))
        .route("/sessions/{id}/recover", post(recover_session))
        .route(
            "/sessions/{id}/github",
            post(refresh_github_session)
                .put(set_github_session)
                .delete(clear_github_session),
        )
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
            "/sessions/{id}/reviews",
            get(list_session_reviews).post(create_session_review),
        )
        .route(
            "/sessions/{id}/artifacts/{name}",
            get(get_artifact)
                .put(write_artifact)
                .delete(delete_artifact),
        )
        .route(
            "/sessions/{id}/artifacts/{name}/raw",
            get(raw_artifact_image),
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
        // Compatibility alias for the branch-owned history endpoint below.
        .route("/sessions/{id}/log", get(branch_events))
        .route("/sessions/{id}/conversation", get(conversation_session))
        .route(
            "/sessions/{id}/conversation/blocks/{message}/{block}",
            get(conversation_block),
        )
        .route("/sessions/{id}/history", get(session_history))
        .route("/sessions/{id}/history/search", get(search_session_history))
        .route("/sessions/{id}/files", get(list_session_files))
        .route("/sessions/{id}/changes", get(get_session_changes))
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
        .route("/sessions/{id}/github-token", post(github_token))
        // The ACP chat journal + live stream, and the ACP drive routes (a
        // `session/prompt` queueing send, a permission answer, a mode change).
        .route("/sessions/{id}/chat", get(get_session_chat))
        .route("/sessions/{id}/chat/stream", get(chat_stream))
        .route(
            "/sessions/{id}/prompt",
            post(prompt_session).delete(retract_queued_prompt),
        )
        .route(
            "/sessions/{id}/permissions/{request_id}",
            post(answer_permission),
        )
        .route("/sessions/{id}/mode", axum::routing::put(set_mode))
        .route(
            "/sessions/{id}/config/{config_id}",
            axum::routing::put(set_config_option),
        )
        .route(
            "/sessions/{id}/tags/{key}",
            axum::routing::put(set_session_tag).delete(clear_session_tag),
        )
        .route("/sessions/{id}/tags", axum::routing::put(set_session_tags))
        // Durable user/agent communication contexts.
        .route("/channels", get(list_channels).post(create_channel))
        .route("/channels/{id}", get(get_channel).delete(archive_channel))
        .route("/channels/{id}/bindings", get(list_channel_bindings))
        .route(
            "/channels/{id}/messages",
            get(list_channel_messages).post(create_channel_message),
        )
        .route(
            "/channels/{id}/subscription",
            axum::routing::put(set_channel_subscription),
        )
        .route(
            "/channels/{id}/read-marker",
            axum::routing::put(set_channel_read_marker),
        )
        // Branches & legacy work items
        .route("/branches", get(list_branches))
        .route("/branches/{id}", get(get_branch).patch(patch_branch))
        // Command routes: each is a multi-write + event sequence the `weaver`
        // CLI needs done atomically server-side, not composed client-side out
        // of generic PATCH calls (which would miss the event or race a
        // partial write). No live session required — `weaver` runs as an
        // HTTP-only client of loom and these are its primary write path.
        .route("/branches/{id}/status", post(set_branch_status))
        .route("/branches/{id}/slack/reply", post(slack_reply))
        .route(
            "/branches/{id}/events",
            get(branch_events).post(create_branch_event),
        )
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
        .route(
            "/reviews/{id}",
            get(get_review).patch(update_review).delete(discard_review),
        )
        .route("/reviews/{id}/comments", post(add_review_comment))
        .route(
            "/reviews/{id}/comments/{comment_id}",
            axum::routing::patch(update_review_comment).delete(delete_review_comment),
        )
        .route(
            "/reviews/{id}/comments/{comment_id}/resolve",
            post(resolve_review_comment),
        )
        .route(
            "/reviews/{id}/retarget-current",
            post(retarget_review_to_current),
        )
        .route("/reviews/{id}/submit", post(submit_review))
        .route("/reviews/{id}/retry-delivery", post(retry_review_delivery))
        // The cross-repo issue board (the loom Issues pane consumes this).
        .route("/issues", get(list_all_issues))
        .route("/issues/actions", post(issue_actions))
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
        .route("/repos/revisions/validate", get(validate_repo_revision))
        .route(
            "/repos/issues",
            get(list_repo_issues).post(create_repo_issue),
        )
        // Per-repo environment variables (write-only values), layered into a
        // non-restricted session's terminal above its selected profile.
        .route("/repos/env", get(get_repo_env))
        .route(
            "/repos/env/{name}",
            axum::routing::put(put_repo_env).delete(delete_repo_env),
        )
        .route("/settings", get(get_settings).patch(patch_settings))
        .route("/deployment/reconcile", post(reconcile_deployment))
        .route("/mcps", get(list_mcps))
        .route(
            "/mcps/custom",
            get(list_custom_mcps).post(create_custom_mcp),
        )
        .route(
            "/mcps/custom/{*identity}",
            get(get_custom_mcp)
                .put(put_custom_mcp)
                .delete(delete_custom_mcp),
        )
        .route("/profiles", get(list_profiles).post(create_profile))
        .route("/profiles/{name}/effective", get(effective_profile))
        .route("/profiles/{name}/probe", post(probe_profile))
        .route("/profiles/{name}/clone", post(clone_profile))
        .route(
            "/profiles/{name}",
            get(get_profile).put(put_profile).delete(delete_profile),
        )
        .route(
            "/profiles/{profile}/env/{name}",
            axum::routing::put(put_profile_env).delete(delete_profile_env),
        )
        .route("/slack/status", get(slack_status))
        // Readable compatibility facade for the default profile's environment.
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
        .route("/diagnostics", get(diagnostics))
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
        .route("/auth/automation-token", post(mint_automation_token))
        .route(
            "/auth/federations",
            get(list_federations).post(add_federation),
        )
        .route("/auth/federations/{id}", delete(remove_federation))
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{id}", get(get_run))
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
        .with_state(state.clone());

    let index = static_dir().join("index.html");
    Router::new()
        // Conventional root scrape endpoint. The public edge may block it
        // while a same-host metrics agent scrapes the loopback listener.
        .route("/metrics", get(metrics))
        .nest("/api", api)
        .fallback_service(ServeDir::new(static_dir()).fallback(ServeFile::new(index)))
        .layer(axum::middleware::from_fn(static_cache_middleware))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        // Outermost, so it wraps auth and every other layer: tag each request's log
        // lines with its method + path (see `request_context_span`).
        .layer(axum::middleware::from_fn(request_context_span))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_error_separates_user_message_from_operator_context() {
        let error = AppError::internal(
            "Could not finish archiving session abc. Retry in a moment.",
            anyhow::anyhow!("docker removal failed"),
        );

        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error.message(),
            "Could not finish archiving session abc. Retry in a moment."
        );
        assert!(error
            .source_chain
            .as_deref()
            .unwrap()
            .contains("docker removal failed"));
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
