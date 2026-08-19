//! axum REST API + SSE. The Vue SPA is the primary consumer.
//!
//! ## The router is derived from declarations
//!
//! Almost every endpoint here is a **registered operation**: it is declared
//! once in `weaver_api::operations`, and its route, method, input schema,
//! actor policy, CLI projection and MCP tool all fall out of that one
//! declaration. The path is mechanical — `issues.list` is
//! `POST /api/issues/list`, `sessions.shells.terminal` is
//! `GET /api/sessions/shells/terminal` — with no separate route table to
//! maintain.
//!
//! Two functions build the surface:
//!
//! * `operations::mount` serves every operation whose response is JSON — the
//!   overwhelming majority — through one dispatcher. Arguments arrive as a JSON
//!   body matching the operation's `Input`; operands are fields, not URL
//!   parameters or query strings. The three `io = Session` operations route
//!   through the same dispatcher: their body is JSON, differing only in the
//!   `Set-Cookie` header, which is why they mount beside the auth routes.
//! * `encodings::mount` serves every `io = Stream | Duplex` operation with
//!   custom handlers — axum requires concrete SSE or websocket-upgrade response
//!   types. Operands arrive in the query string, which is the sole place where
//!   encoding affects the wire format.
//!
//! The remainder mounts by hand, tracked by `crates/loom/tests/surface_parity.rs`:
//! what is listed there comprises transport that is not operation-backed
//! (the code-server proxy, liveness probes, the GitHub webhook, OAuth redirects),
//! routes that operations have superseded but whose callers have not yet migrated,
//! and routes with no operation at all. The test enforces that this file and the
//! ledger remain synchronized.
//!
//! Response encoding is the only thing an operation's declaration varies about
//! its transport; see `docs/ARCHITECTURE.md` for the projection table.
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
//!     "description": "...",         // current-state message (loom status)
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
//! A branch's status axes — the agent's self-reported `attention` and a
//! watch's `triage` — are **tags**: well-known keys under `tags`, set through
//! `sessions.tags.set` and cleared through `sessions.tags.delete`. Absence is
//! the calm state; there is no stored `ok` tag.

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
mod encodings;
mod env;
mod eventmux;
mod github_access;
mod issues;
mod launches;
mod logview;
mod mcps;
mod operations;
mod permission_requests;
mod profiles;
mod repo_env;
mod repos;
mod restricted_github;
mod reviews;
mod scope;
mod scratch;
mod self_context;
mod session_layout;
mod session_summary;
pub(crate) mod sessions;
mod settings;
mod watches;

use artifacts::*;
use auth::*;
use automation::*;
use channels::*;
use diagnostics::*;
use github_access::*;
use issues::*;
use operations::*;
use repos::*;
use restricted_github::*;
pub(crate) use scope::{require_branch_access, require_repo_access, require_session_access};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use axum::{
    body::HttpBody as _,
    extract::{DefaultBodyLimit, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
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

pub(super) async fn configured_github_app(
    st: &AppState,
) -> ApiResult<&crate::github_app::GithubApp> {
    let app = st.trigger.app().ok_or_else(|| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub App credential is unavailable",
        )
    })?;
    if !app.is_configured().await {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub App credential is unavailable",
        ));
    }
    Ok(app)
}
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
pub(super) fn is_ide_proxy_path(path: &str) -> bool {
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

fn registered_api_router() -> Router<AppState> {
    // Declarations and handlers must be the same set, and the moment to find
    // out is boot — not the first request to a descriptor nothing serves. The
    // registry this replaces had no such check, which is how it came to
    // advertise operations that 404ed.
    operations::assert_registry_is_complete();
    let router = operations::mount(Router::new());
    // The non-JSON half of the same registry: SSE feeds and terminal
    // websockets, mounted at their derived paths off the same declarations.
    let router = encodings::mount(router);
    // The IDE proxy: the one hand-mounted endpoint on the authenticated surface.
    // It forwards an arbitrary sub-path into a container and streams back
    // whatever comes out — it has no operation declaration and will not get one.
    router
        .route("/sessions/{id}/ide", axum::routing::any(crate::ide::proxy))
        .route("/sessions/{id}/ide/", axum::routing::any(crate::ide::proxy))
        .route(
            "/sessions/{id}/ide/{*rest}",
            axum::routing::any(crate::ide::proxy),
        )
        // Registry discovery. Operations describing operations would be
        // circular, so these four stay routes.
        .route("/meta", get(api_meta))
        .route("/operations", get(list_operations))
        .route("/operations/{id}", get(get_operation))
        .route("/openapi.json", get(openapi))
}

/// The unauthenticated route table.
///
/// Split out from [`router`] so the whole surface can be built — and therefore
/// checked for overlapping routes — without a database. See the tests below.
fn public_api_router() -> Router<AppState> {
    // Public surface: the liveness probe and the login flow itself. No
    // middleware — these must work for an unauthenticated caller, since they are
    // how one *becomes* authenticated.
    Router::new()
        // `/health` remains the compatibility liveness probe. `/health/live`
        // names it explicitly; readiness checks DB + migration state.
        .route("/health", get(liveness))
        .route("/health/live", get(liveness))
        .route("/ready", get(readiness))
        .route("/health/ready", get(readiness))
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .route("/auth/github/login", get(github_login))
        .route("/auth/github/callback", get(github_callback))
        .route("/auth/federate", post(federate))
        // The inbound GitHub webhook. Deliberately OUTSIDE `require_auth`: it is
        // authenticated cryptographically by the HMAC signature it carries, not
        // by a loom principal. The handler is the untrusted-input boundary.
        .route("/github/webhook", post(github_webhook))
}

/// The authenticated route table: every registered operation plus the
/// hand-written routes that are not operations yet.
///
/// Takes no state for the same reason [`public_api_router`] does not — building
/// it is how route overlaps are detected, and that must not need a database.
fn protected_api_router() -> Router<AppState> {
    // Every endpoint here requires an authenticated principal — a bearer token, a
    // session cookie, or a trusted-loopback request — gated by `require_auth`.
    //
    // There is nothing to add to this function. Adding an endpoint means
    // declaring an operation in `weaver-api` and binding it with
    // `register::<O>(handler)`; the route follows from the id. What used to sit
    // here was a second route table for the same data — 121 hand-written routes
    // whose paths, methods, bodies and authority were maintained separately from
    // the declarations that claimed to describe them.
    registered_api_router()
}

pub fn router(state: AppState) -> Router {
    let protected = protected_api_router()
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        // Scratch uploads can carry images / logs; lift the default 2 MB cap.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024));

    let api = public_api_router()
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

#[cfg(test)]
mod route_table_tests {
    /// The whole route table builds.
    ///
    /// axum panics at `.route()` when two handlers claim the same method and
    /// path, so *constructing* the router is the check — and it covers every
    /// overlap, not just the ones a source-scanning ledger can see.
    ///
    /// This is not hypothetical. `POST /deployment/reconcile` was mounted by
    /// hand *and* derived from the `deployment.reconcile` operation. Nothing
    /// caught it: the surface ledger counted a hand-mounted route that equals an
    /// operation's route as "accounted for", which is exactly backwards. The
    /// panic happened inside each integration test's server task, so all 185 of
    /// them failed with "cannot reach loom" and none of them said why.
    #[test]
    fn the_route_table_has_no_overlapping_routes() {
        let _ = super::public_api_router();
        let _ = super::protected_api_router();
        // The merge is where a public route can collide with a protected one.
        let _ = super::public_api_router().merge(super::protected_api_router());
    }
}
