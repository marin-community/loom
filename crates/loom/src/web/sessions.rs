use std::collections::HashSet;
use std::convert::Infallible;
use std::path::{Component, PathBuf};
use std::pin::Pin;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{self, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::auth::Principal;
use crate::db::Db;
use crate::events::Event;
use crate::session::{self as session_mod, Session};
use crate::{agent, backend, config, custom_agents, db, events, git, github, repo};
use base64::Engine as _;
use weaver_api::operations::sessions as ops;
use weaver_api::{
    AcpMetadataView, BranchView, ChatBlockView, ChatCursorView, CreateReq, EnsureResumptionCueReq,
    HandoffReq, HistoryPageView, PatchSessionReq, ResolvedLaunchView, ResumptionCueView,
    SearchSessionsOptions, SendReq, SessionArchiveResult, SessionChatView, SessionCreatorFilter,
    SessionFilesView, SessionIdeInfoView, SessionInterruptResult, SessionModeResult,
    SessionPreviewResult, SessionRawFileView, SessionSearchAttention, SessionSearchStatus,
    SessionSendResult, SessionSummaryView, SessionUrlView, SessionView, SetTagsReq,
    SetTitleGenerationReq, TagReq,
};
use weaver_core::branch as branch_mod;
use weaver_core::branch::{Branch, TitleProvenance, TitleUpdate};
use weaver_core::tags;
use weaver_core::watch::{self as watch_store};

use super::operations::{register, Bound, OperationContext};
use super::{
    author_or_manual, require_branch, require_session, session_summary_view, session_view,
};
use super::{ApiResult, AppError, AppState};
use crate::lifecycle::{
    adopt, adopt_acp, archive, delete_session_row, require_branch_slot_free, require_no_transition,
    require_resume_capacity, require_session_profile_lifetime, resume_agent, stamped_custom_agent,
};

pub(super) async fn list_agents(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let default_agent = crate::profile::get(&st.db, crate::profile::DEFAULT_PROFILE)
        .await?
        .map(|profile| profile.agent_kind)
        .unwrap_or_else(|| config::DEFAULT_AGENT.to_string());
    Ok(Json(json!({
        // The picker list (builtins + custom) and the full custom-agent
        // definitions the editor round-trips.
        "agents": agent::agent_metadata(&st.db).await?,
        "custom": custom_agents::list(&st.db).await?,
        "default_agent": default_agent,
    })))
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

/// Query for `GET /api/sessions`: trim the fleet listing for the caller.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ListSessionsQuery {
    /// Include archived (torn-down) sessions. Defaults to `false` — an archived
    /// session is out of the active fleet, so the agent's `loom session ls` and
    /// any survey see only live work unless they ask. The SPA fetches the
    /// opt-in inventory once, then projects active Workspace and archived
    /// History as disjoint views.
    #[serde(default)]
    archived: bool,
    /// Compatibility filter for automation-class sessions. With no creator
    /// scope, omission retains the historical interactive-only inventory;
    /// explicit creator scopes select the classes they name directly.
    #[serde(default)]
    automation: Option<bool>,
    /// Include engine-managed warm sessions. This is an operator inventory
    /// escape hatch: normal fleet/survey callers must not see a watcher's own
    /// infrastructure and recurse into it.
    #[serde(default)]
    managed: bool,
    /// Case-insensitive substring filter over a session's title, branch name,
    /// and goal (`loom session ls --search auth`). Absent/blank matches everything.
    #[serde(default)]
    q: Option<String>,
    /// Viewer-relative creator scope (`mine`, `ops`, their union, or other users).
    #[serde(default)]
    creator: Option<SessionCreatorFilter>,
}

pub(super) async fn list_sessions(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<ListSessionsQuery>,
) -> ApiResult<Json<Vec<SessionView>>> {
    if q.managed && !principal.is_human() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "human grant required to list managed sessions",
        ));
    }
    collect_sessions(
        &st,
        q.managed,
        SessionCollectionFilter {
            archived: q.archived,
            archived_only: false,
            automation: q.automation.unwrap_or(false),
            search: q.q.as_deref(),
            status: None,
            attention: None,
            creator: q.creator,
            viewer: &principal.username,
        },
    )
    .await
    .map(Json)
}

/// Compact fleet/search query for `GET /api/sessions/summary`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ListSessionSummariesQuery {
    /// Include archived rows alongside active work.
    #[serde(default)]
    archived: bool,
    /// Return only archived rows. Implies `archived=true`.
    #[serde(default)]
    archived_only: bool,
    /// Include automation-class sessions.
    #[serde(default)]
    automation: bool,
    /// Case-insensitive search over the same documented facets as fleet search.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    status: Option<SessionSearchStatus>,
    #[serde(default)]
    attention: Option<SessionSearchAttention>,
    #[serde(default)]
    creator: Option<SessionCreatorFilter>,
}

/// Compact polling/search contract for indexes. Full session context remains on
/// `GET /api/sessions/{id}` and is fetched only when a row or page discloses it.
pub(super) async fn list_session_summaries(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<ListSessionSummariesQuery>,
) -> ApiResult<Json<Vec<SessionSummaryView>>> {
    collect_session_summaries(
        &st,
        SessionCollectionFilter {
            archived: q.archived || q.archived_only,
            archived_only: q.archived_only,
            automation: q.automation,
            search: q.q.as_deref(),
            status: q.status,
            attention: q.attention,
            creator: q.creator,
            viewer: &principal.username,
        },
    )
    .await
    .map(Json)
}

/// Search is an explicit fleet read so non-browser clients can find the same
/// qualified sessions as the workbench. `history=true` widens the actionable
/// result set; `archived_only=true` is the disjoint History projection.
pub(super) async fn search_sessions(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<SearchSessionsOptions>,
) -> ApiResult<Json<Vec<SessionView>>> {
    collect_sessions(
        &st,
        false,
        SessionCollectionFilter {
            archived: q.history || q.archived_only,
            archived_only: q.archived_only,
            automation: true,
            search: Some(&q.query),
            status: q.status,
            attention: q.attention,
            creator: q.creator,
            viewer: &principal.username,
        },
    )
    .await
    .map(Json)
}

fn attention_level<'a>(status: &'a str, tags: &[weaver_api::TagView]) -> &'a str {
    if status == "archived" {
        "ok"
    } else if tags.iter().any(|tag| tag.value == "blocked") {
        "blocked"
    } else if matches!(status, "error" | "orphaned")
        || tags.iter().any(|tag| tag.value == "attention")
    {
        "attention"
    } else {
        "ok"
    }
}

fn matches_attention(
    status: &str,
    tags: &[weaver_api::TagView],
    filter: SessionSearchAttention,
) -> bool {
    let level = attention_level(status, tags);
    match filter {
        SessionSearchAttention::Needs => level != "ok",
        SessionSearchAttention::Ok => level == "ok",
        SessionSearchAttention::Attention => level == "attention",
        SessionSearchAttention::Blocked => level == "blocked",
    }
}

fn append_search_field(haystack: &mut String, value: &str) {
    haystack.push(' ');
    haystack.push_str(value);
}

struct SessionSearchFacets<'a> {
    placement: Option<&'a weaver_api::SessionPlacementView>,
    title: &'a str,
    goal: &'a str,
    description: &'a str,
    repo_root: &'a str,
    branch: &'a str,
    name: &'a str,
    base_branch: &'a str,
    github_repo: Option<&'a str>,
    status: &'a str,
    profile: &'a str,
    origin: &'a str,
    class: &'a str,
    created_by: Option<&'a str>,
    parent_session_id: Option<&'a str>,
    parent_id: Option<&'a str>,
    github_issue: Option<&'a weaver_api::GithubIssueRef>,
    tracking_issue: Option<i64>,
    github: Option<&'a weaver_core::github::GithubStatus>,
    github_pr: Option<i64>,
    tags: &'a [weaver_api::TagView],
}

fn search_haystack(facets: SessionSearchFacets<'_>) -> String {
    let mut haystack = String::new();
    if let Some(placement) = facets.placement {
        append_search_field(
            &mut haystack,
            &format!("{} / {}", placement.group_name, facets.title.trim()),
        );
        append_search_field(&mut haystack, &placement.group_name);
    }
    for field in [
        facets.title,
        facets.goal,
        facets.description,
        facets.repo_root,
        facets.branch,
        facets.name,
        facets.base_branch,
        facets.github_repo.unwrap_or_default(),
        facets.status,
        facets.profile,
        facets.origin,
        facets.class,
        facets.created_by.unwrap_or_default(),
        facets.parent_session_id.unwrap_or_default(),
        facets.parent_id.unwrap_or_default(),
    ] {
        append_search_field(&mut haystack, field);
    }
    if let Some(issue) = facets.github_issue {
        append_search_field(&mut haystack, &format!("{}#{}", issue.repo, issue.number));
        append_search_field(&mut haystack, &format!("#{}", issue.number));
    }
    if let Some(issue) = facets.tracking_issue {
        append_search_field(&mut haystack, &format!("#{issue}"));
    }
    if let Some(pr) = facets.github {
        append_search_field(&mut haystack, &format!("#{}", pr.pr_number));
        for field in [
            pr.pr_url.as_str(),
            pr.pr_title.as_str(),
            pr.pr_state.as_str(),
            pr.review_decision.as_deref().unwrap_or_default(),
            pr.checks.as_deref().unwrap_or_default(),
        ] {
            append_search_field(&mut haystack, field);
        }
    } else if let Some(pr) = facets.github_pr {
        append_search_field(&mut haystack, &format!("#{pr}"));
    }
    for tag in facets.tags {
        for field in [
            tag.key.as_str(),
            tag.value.as_str(),
            tag.note.as_str(),
            tag.set_by.as_str(),
        ] {
            append_search_field(&mut haystack, field);
        }
    }
    haystack.to_lowercase()
}

fn view_search_haystack(view: &SessionView) -> String {
    search_haystack(SessionSearchFacets {
        placement: view.placement.as_ref(),
        title: &view.branch.title,
        goal: &view.branch.goal,
        description: &view.branch.description,
        repo_root: &view.branch.repo_root,
        branch: &view.branch.branch,
        name: &view.branch.name,
        base_branch: &view.branch.base_branch,
        github_repo: view.github_repo.as_deref(),
        status: &view.status,
        profile: &view.profile,
        origin: &view.origin,
        class: &view.class,
        created_by: view.created_by.as_deref(),
        parent_session_id: view.parent_session_id.as_deref(),
        parent_id: view.parent_id.as_deref(),
        github_issue: view.github_issue.as_ref(),
        tracking_issue: view.tracking_issue,
        github: view.branch.github.as_ref(),
        github_pr: view.branch.github_pr,
        tags: &view.branch.tags,
    })
}

fn summary_search_haystack(view: &SessionSummaryView, branch: &Branch) -> String {
    search_haystack(SessionSearchFacets {
        placement: view.placement.as_ref(),
        title: &view.branch.title,
        goal: &branch.goal,
        description: &view.branch.description,
        repo_root: &view.branch.repo_root,
        branch: &view.branch.branch,
        name: &view.branch.name,
        base_branch: &branch.base_branch,
        github_repo: view.github_repo.as_deref(),
        status: &view.status,
        profile: &view.profile,
        origin: &view.origin,
        class: &view.class,
        created_by: view.created_by.as_deref(),
        parent_session_id: view.parent_session_id.as_deref(),
        parent_id: view.parent_id.as_deref(),
        github_issue: view.github_issue.as_ref(),
        tracking_issue: view.tracking_issue,
        github: view.branch.github.as_ref(),
        github_pr: view.branch.github_pr,
        tags: &view.branch.tags,
    })
}

struct SessionCollectionFilter<'a> {
    archived: bool,
    archived_only: bool,
    automation: bool,
    search: Option<&'a str>,
    status: Option<SessionSearchStatus>,
    attention: Option<SessionSearchAttention>,
    creator: Option<SessionCreatorFilter>,
    viewer: &'a str,
}

fn search_needle(filter: &SessionCollectionFilter<'_>) -> Option<String> {
    filter
        .search
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(str::to_lowercase)
}

fn session_in_collection(
    session: &Session,
    warm: &HashSet<String>,
    filter: &SessionCollectionFilter<'_>,
    managed: bool,
) -> bool {
    let mine = session.created_by.as_deref() == Some(filter.viewer);
    let ops = session.class == "automation";
    let other_user = session
        .created_by
        .as_deref()
        .is_some_and(|creator| creator != filter.viewer);
    let creator_matches = filter.creator.is_none_or(|creator| match creator {
        SessionCreatorFilter::Mine => mine,
        SessionCreatorFilter::Ops => ops,
        SessionCreatorFilter::MineAndOps => mine || ops,
        SessionCreatorFilter::OtherUsers => other_user && !ops,
    });
    (managed || !warm.contains(&session.id))
        && (filter.archived || session.status != "archived")
        && (!filter.archived_only || session.status == "archived")
        && (filter.automation || filter.creator.is_some() || session.class != "automation")
        && creator_matches
}

fn view_matches_filters(
    status: &str,
    tags: &[weaver_api::TagView],
    filter: &SessionCollectionFilter<'_>,
) -> bool {
    filter.status.is_none_or(|wanted| status == wanted.as_str())
        && filter
            .attention
            .is_none_or(|wanted| matches_attention(status, tags, wanted))
}

async fn collect_sessions(
    st: &AppState,
    managed: bool,
    filter: SessionCollectionFilter<'_>,
) -> ApiResult<Vec<SessionView>> {
    // The fleet listing shows work, not infrastructure: engine-managed (warm)
    // sessions are excluded here, so neither the dashboard nor a watch
    // round's survey (scripts read this route) ever sees a watcher's own
    // session — the no-recursion guarantee. `list_visible` drops `managed_by`
    // rows; the `warm_session_id` check below is belt-and-braces for a warm
    // session not yet stamped. Internal liveness/adopt paths use
    // `session::list` instead.
    let warm: std::collections::HashSet<String> = watch_store::list(&st.db)
        .await?
        .into_iter()
        .filter_map(|o| o.warm_session_id)
        .collect();
    // A blank `q` is no filter; otherwise match case-insensitively.
    let needle = search_needle(&filter);
    let sessions = if managed {
        session_mod::list(&st.db).await?
    } else {
        session_mod::list_visible(&st.db).await?
    };
    let mut views: Vec<SessionView> = Vec::with_capacity(sessions.len());
    for s in sessions {
        if !session_in_collection(&s, &warm, &filter, managed) {
            continue;
        }
        if let Some(branch) = branch_mod::get(&st.db, &s.branch_id).await? {
            let view = session_view(&st.db, &s, &branch).await?;
            if !view_matches_filters(&view.status, &view.branch.tags, &filter) {
                continue;
            }
            if let Some(needle) = &needle {
                // The wire view already carries every promised search facet:
                // qualified placement, title/goal, repo/branch, issue/PR, tags,
                // status, profile, and provenance. Searching only its values
                // keeps that vocabulary synchronized without matching JSON keys.
                let hay = view_search_haystack(&view);
                if !hay.contains(needle) {
                    continue;
                }
            }
            views.push(view);
        }
    }
    Ok(views)
}

async fn collect_session_summaries(
    st: &AppState,
    filter: SessionCollectionFilter<'_>,
) -> ApiResult<Vec<SessionSummaryView>> {
    let warm: HashSet<String> = watch_store::list(&st.db)
        .await?
        .into_iter()
        .filter_map(|watch| watch.warm_session_id)
        .collect();
    let needle = search_needle(&filter);
    let sessions = session_mod::list_visible(&st.db).await?;
    let mut views = Vec::with_capacity(sessions.len());
    for session in sessions {
        if !session_in_collection(&session, &warm, &filter, false) {
            continue;
        }
        let Some(branch) = branch_mod::get(&st.db, &session.branch_id).await? else {
            continue;
        };
        let view = session_summary_view(&st.db, &session, &branch).await?;
        if !view_matches_filters(&view.status, &view.branch.tags, &filter) {
            continue;
        }
        if let Some(needle) = &needle {
            let haystack = summary_search_haystack(&view, &branch);
            if !haystack.contains(needle) {
                continue;
            }
        }
        views.push(view);
    }
    Ok(views)
}

pub(super) async fn get_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// `GET /api/sessions/{id}/url` — the dashboard URL for a session.
///
/// The agent inside a session can't build this itself: it only knows the
/// loopback `$WEAVER_API` it was handed, and a `http://127.0.0.1:7878/…` link
/// pasted into a PR is useless to whoever reads it. Only the server knows the
/// externally-visible origin (the operator's `auth.base_url`, else the request's
/// own Host), so resolving it is the server's job — see `loom session url`.
pub(super) async fn session_url_route(
    State(st): State<AppState>,
    headers: header::HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let base = super::auth::public_base(&st, &headers).await;
    Ok(Json(
        json!({ "url": crate::links::session_url(&base, &session.id) }),
    ))
}

pub(super) async fn create_session(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(mut req): Json<CreateReq>,
) -> ApiResult<Json<SessionView>> {
    // The authenticated managed-repository convenience below is itself a
    // durable side effect. Reject the complete untrusted Scratch batch first.
    let _ = crate::scratch::prepare_initial_scratch(&req.scratch)
        .map_err(super::scratch::map_scratch_error)?;
    // Naming a managed repo here registers it: a signed-in principal asking to
    // launch into `owner/name` is the grant, so a repo loom has never seen just
    // works (it is cloned on the way through runtime provisioning). The `repos`
    // allowlist exists to gate the *unauthenticated* GitHub webhook, which
    // resolves its own clone against it before it ever reaches the shared core —
    // so admitting a repo on an authenticated launch leaves that boundary intact.
    if let Some(input) = req.repo.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        ensure_repo_registered(&st.db, input).await?;
    }
    // Attribute the session to whoever the auth middleware resolved: a human
    // (cookie/token) → their username; a loopback/local-token call → the owner;
    // a future webhook → its bot principal. Read from the `Principal`, never
    // hardcoded and never client-supplied.
    //
    // A launch that names a parent branch is an agent delegating work; a plain
    // launch is the human's own. The GitHub/Slack trigger paths stamp their own
    // origin at their call sites.
    let delegated = req
        .parent_branch
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    // `None` means the credential cannot launch anything at all. `authorize()`
    // already refuses an anonymous grant here, so this fails closed rather than
    // depending on that.
    let actor =
        crate::provision::Actor::from_principal(&principal, delegated).ok_or_else(|| {
            AppError::new(StatusCode::FORBIDDEN, "credential cannot launch a session")
        })?;
    match &principal.grant {
        crate::auth::Grant::Session { .. } => {
            req.parent_branch = actor.bound_parent_branch().map(str::to_string);
        }
        crate::auth::Grant::Automation { .. } => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "automation credentials create sessions through /api/runs",
            ));
        }
        // Unreachable: `from_principal` above already returned `None`.
        crate::auth::Grant::Anonymous => {}
        crate::auth::Grant::Admin | crate::auth::Grant::User => {}
    }
    let created = crate::provision::create(st.clone(), req, actor)
        .await
        .map_err(super::provision_error)?;
    Ok(Json(
        session_view(&st.db, &created.session, &created.branch).await?,
    ))
}

/// Add a managed-repo reference to the registry if it isn't there yet — the same
/// slug → (remote, managed path) mapping `POST /api/repos` writes. Idempotent: a
/// repo already registered keeps the remote it was registered with.
async fn ensure_repo_registered(db: &Db, input: &str) -> ApiResult<()> {
    let slug = repo::parse_slug(input).map_err(AppError::bad_request)?;
    if repo::get_registered(db, &slug.slug()).await?.is_some() {
        return Ok(());
    }
    let path = slug.path(&repo::repos_dir());
    repo::register(
        db,
        &slug.slug(),
        &repo::remote_url_for(input, &slug),
        &path.to_string_lossy(),
    )
    .await?;
    Ok(())
}

fn map_handoff_error(error: crate::handoff::HandoffError) -> AppError {
    use crate::handoff::HandoffError::*;
    let (status, message, preview) = match error {
        BadRequest(message) => (StatusCode::BAD_REQUEST, message, None),
        Forbidden(message) => (StatusCode::FORBIDDEN, message, None),
        NotFound(message) => (StatusCode::NOT_FOUND, message, None),
        Conflict(message, preview) => (
            StatusCode::CONFLICT,
            message,
            preview.map(|preview| *preview),
        ),
        PreconditionRequired(message) => (StatusCode::PRECONDITION_REQUIRED, message, None),
        Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message, None),
    };
    let mut mapped = AppError::new(status, message);
    if let Some(preview) = preview {
        mapped = mapped.with_fields(json!({ "preview": preview }));
    }
    mapped
}

pub(super) async fn resolve_session_handoff(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<weaver_api::ResolveLaunchReq>,
) -> ApiResult<Json<weaver_api::ResolvedLaunchView>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let view = crate::handoff::resolve_session_handoff(&st, &session, &req.selection)
        .await
        .map_err(map_handoff_error)?;
    Ok(Json(view))
}

/// Set (upsert) a tag on a session's branch: validate `value` against the key's
/// ladder, write the tag, and broadcast a `tag` event. The well-known keys are
/// `attention` (the agent's self-report) and `triage` (a watch's, or a
/// hand operator's, assessment); any other key is a free-form quiet pill. To
/// return a loud key to calm, `DELETE` the tag rather than setting an `ok` value.
pub(super) async fn set_session_tag(
    State(st): State<AppState>,
    Path((key, tag_key)): Path<(String, String)>,
    Json(req): Json<TagReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let value = req.value.trim();
    if crate::github::is_reserved_tag(&tag_key) {
        return Err(AppError::bad_request(format!(
            "'{tag_key}' is loom-internal bookkeeping — it can be cleared, not set by hand"
        )));
    }
    // Same wiring-format gate as the branch-scoped route: the status-card
    // mirror consumes this value, so a typo must fail loudly at set time.
    if tag_key == tags::GITHUB_KEY && crate::github::parse_wiring(value).is_none() {
        return Err(AppError::bad_request(format!(
            "invalid value '{value}' for '{tag_key}' — expected owner/name#number"
        )));
    }
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

/// Atomically replace one author's complete tag set on a session branch.
///
/// This is the watch-safe counterpart to the per-key routes: rows still
/// authored by `by` are replaced as one transaction, so a stale round cannot
/// DELETE a key another actor took over after its fleet snapshot. Exact-match
/// `clear` entries let a real status replace lifecycle marks such as
/// `(idle, idle)` without making a key-only stale delete.
pub(super) async fn set_session_tags(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SetTagsReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let by = author_or_manual(req.by.as_deref());
    let mut seen = HashSet::new();
    let mut desired = Vec::with_capacity(req.tags.len());
    for tag in req.tags {
        let tag_key = tag.key.trim();
        let value = tag.value.trim();
        if tag_key.is_empty() {
            return Err(AppError::bad_request("tag key must be non-empty"));
        }
        if !seen.insert(tag_key.to_string()) {
            return Err(AppError::bad_request(format!(
                "duplicate tag key '{tag_key}'"
            )));
        }
        if crate::github::is_reserved_tag(tag_key) {
            return Err(AppError::bad_request(format!(
                "'{tag_key}' is loom-internal bookkeeping — it can be cleared, not set by hand"
            )));
        }
        if tag_key == tags::GITHUB_KEY && crate::github::parse_wiring(value).is_none() {
            return Err(AppError::bad_request(format!(
                "invalid value '{value}' for '{tag_key}' — expected owner/name#number"
            )));
        }
        if !tags::is_valid_value(tag_key, value) {
            return Err(AppError::bad_request(if tags::is_loud(tag_key) {
                format!(
                    "invalid value '{value}' for '{tag_key}' — expected one of {} (omit the tag to return to calm)",
                    tags::ATTENTION_VALUES.join(", ")
                )
            } else {
                format!("invalid value '{value}' for '{tag_key}' — must be non-empty")
            }));
        }
        desired.push(tags::TagInput {
            key: tag_key.to_string(),
            value: value.to_string(),
            note: tag.note.trim().to_string(),
        });
    }
    let mut clear = Vec::with_capacity(req.clear.len());
    for tag in req.clear {
        let tag_key = tag.key.trim();
        let value = tag.value.trim();
        if tag_key.is_empty() || value.is_empty() {
            return Err(AppError::bad_request(
                "exact tag clears require non-empty key and value",
            ));
        }
        clear.push(tags::TagMatch {
            key: tag_key.to_string(),
            value: value.to_string(),
        });
    }

    let replaced = tags::replace_by(&st.db, &branch.id, &by, &desired, &clear).await?;
    for old in &replaced.before {
        if !replaced.after.iter().any(|new| new.key == old.key) {
            events::record_tag(&st.db, &st.bus, &branch.id, &old.key, "", "", &by)
                .await
                .ok();
        }
    }
    for new in &replaced.after {
        let unchanged = replaced.before.iter().any(|old| {
            old.key == new.key
                && old.value == new.value
                && old.note == new.note
                && old.set_by == new.set_by
        });
        if !unchanged {
            events::record_tag(
                &st.db,
                &st.bus,
                &branch.id,
                &new.key,
                &new.value,
                &new.note,
                &new.set_by,
            )
            .await
            .ok();
        }
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Clear a tag on a session's branch — delete the row and broadcast a `tag`
/// event with an empty value (the cleared signal). How a loud axis returns to
/// calm (`ok`). A no-op when the tag is already absent. DELETE carries no
/// body, so the author rides the `by` query parameter (a watch name),
/// defaulting to `manual`.
pub(super) async fn clear_session_tag(
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
pub(crate) struct ByQuery {
    #[serde(default)]
    pub(crate) by: Option<String>,
}

pub(super) async fn patch_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchSessionReq>,
) -> ApiResult<Json<SessionView>> {
    if req.park.is_some() || req.sort_order.is_some() {
        return Err(AppError::bad_request(
            "park and sort_order are read-only compatibility fields; use the revisioned session-layout move API",
        ));
    }
    let (initial_session, _) = require_session(&st.db, &key).await?;
    let _source_permit = st.launch_gate.acquire_session(&initial_session.id).await;
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((session, branch)) = session_mod::with_branch(&st.db, &initial_session.id).await?
    else {
        return Err(AppError::conflict(
            "session changed while the update was waiting; review it again",
        ));
    };
    if let Some(title) = &req.title {
        let title = branch_mod::sanitize_user_title(title)
            .ok_or_else(|| AppError::bad_request("title must not be empty"))?;
        let expected_title = req.expected_title.as_deref().ok_or_else(|| {
            AppError::bad_request("expected_title is required when renaming a session")
        })?;
        let expected_provenance = req
            .expected_title_provenance
            .as_deref()
            .ok_or_else(|| {
                AppError::bad_request(
                    "expected_title_provenance is required when renaming a session",
                )
            })?
            .parse::<TitleProvenance>()
            .map_err(AppError::bad_request)?;
        match branch_mod::replace_title(
            &st.db,
            &branch.id,
            expected_title,
            expected_provenance,
            &title,
            TitleProvenance::User,
        )
        .await?
        {
            TitleUpdate::Applied(_) => {
                crate::channels::update_branch_channel_names(&st.db, &branch.id, &title).await?;
            }
            TitleUpdate::Stale(current) => {
                return Err(AppError::conflict(
                    "the task label changed while it was being edited; review it and retry",
                )
                .with_fields(json!({ "branch": super::branch_view(&st.db, &current).await? })));
            }
            TitleUpdate::Missing => return Err(AppError::not_found("branch")),
        }
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
        session_mod::bump_mutation_revision(&st.db, &session.id).await?;
        crate::channels::update_session_goal(&st.db, &session.id, goal).await?;
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

pub(super) async fn regenerate_session_title(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    crate::metadata_assist::spawn_title_generation(
        st.db.clone(),
        st.bus.clone(),
        st.acp.clone(),
        session.clone(),
        branch,
        true,
    )
    .await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

pub(super) async fn set_session_title_generation(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SetTitleGenerationReq>,
) -> ApiResult<Json<SessionView>> {
    let (session, _) = require_session(&st.db, &key).await?;
    crate::metadata_assist::set_title_enabled(&st.db, &session.id, req.enabled).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

pub(super) async fn get_resumption_cue(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<ResumptionCueView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(
        crate::metadata_assist::current_cue(&st.db, &session, &branch).await?,
    ))
}

pub(super) async fn ensure_resumption_cue(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<EnsureResumptionCueReq>,
) -> ApiResult<Json<ResumptionCueView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(
        crate::metadata_assist::ensure_cue(&st.db, &st.acp, &session, &branch, req.force).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteQuery {
    #[serde(default)]
    keep_branch: bool,
}

pub(super) async fn delete_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = match require_session(&st.db, &key).await {
        Ok(found) => found,
        Err(error) if error.is_not_found() => {
            return delete_launch_attempt(&st, &key).await;
        }
        Err(error) => return Err(error),
    };
    let warnings = remove(&st, &session, &branch, q.keep_branch).await?;
    Ok(Json(json!({
        "deleted": true,
        "kind": "session",
        "warnings": warnings
    })))
}

/// Fully remove one durable session and all resources it owns.
///
/// Shared by the session route and automation cancellation races. External
/// teardown is intentionally idempotent: a missing terminal/worktree is
/// already removed, while failures are returned as warnings after the durable
/// ownership rows have been released.
pub(crate) async fn remove(
    st: &AppState,
    session: &Session,
    _branch: &Branch,
    keep_branch: bool,
) -> Result<Vec<String>, AppError> {
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        // A competing remove already achieved the requested end state.
        return Ok(Vec::new());
    };
    remove_locked(st, &current_session, &current_branch, keep_branch).await
}

/// Shared deletion after the caller has acquired the runtime lifecycle lock and
/// refreshed the session row.
async fn remove_locked(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    keep_branch: bool,
) -> Result<Vec<String>, AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, keep_branch, "deleting session");
    let mut warnings: Vec<String> = Vec::new();

    // Cancellation is the durable boundary: an automation request that
    // finishes provisioning after this point cannot promote itself.
    crate::runs::cancel_for_session(&st.db, &session.id).await?;
    if let Err(error) = backend::kill_session_and_wait(&session.term_session).await {
        warnings.push(format!("terminal remove: {error}"));
    }
    if session.protocol == "acp" {
        st.acp.stop(&session.id);
    }
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    tracing::debug!(session = %session.id, "killed terminal, debug shells, and ide sessions");
    if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
        warnings.push(format!("worktree remove: {e}"));
        tokio::fs::remove_dir_all(&work_dir).await.ok();
    }
    if !keep_branch {
        tracing::debug!(session = %session.id, branch_name = %branch.branch, "deleting git branch");
        if let Err(e) = git::delete_branch(&repo_root, &branch.branch).await {
            warnings.push(format!("delete branch: {e}"));
        }
    }
    tokio::fs::remove_dir_all(db::run_dir(&session.id))
        .await
        .ok();
    crate::auth::revoke_session_tokens(&st.db, &session.id).await?;
    delete_session_row(st, &session.id).await?;
    // Release this branch's claimed issues back to the repo backlog before the
    // branch row goes away — issues are repo-owned and must outlive teardown.
    weaver_core::issue::unclaim_branch(&st.db, &branch.repo_root, &branch.branch)
        .await
        .ok();
    // Drop the branch row too — deleting a session takes its branch with it.
    branch_mod::delete(&st.db, &branch.id).await?;
    if warnings.is_empty() {
        tracing::info!(session = %session.id, branch = %branch.id, keep_branch, "session deleted");
    } else {
        tracing::warn!(branch = %branch.id, warnings = warnings.len(), "session removed with warnings");
    }
    Ok(warnings)
}

/// Remove a launch reservation that failed before a `sessions` row existed.
///
/// `loom session rm <reserved-session-id>` therefore has the same escape hatch
/// as a real session. Terminalize first so an in-flight create cannot promote
/// the run after the operator removes it.
async fn delete_launch_attempt(st: &AppState, session_id: &str) -> ApiResult<Json<Value>> {
    if crate::runs::list_for_session(&st.db, session_id)
        .await?
        .is_empty()
    {
        return Err(AppError::not_found("session"));
    }
    crate::runs::cancel_for_session_with_summary(
        &st.db,
        session_id,
        "launch attempt removed by user",
    )
    .await?;
    let warnings = crate::session_manager::teardown_reserved_runtime(session_id).await;
    st.ide.kill(session_id);
    crate::auth::revoke_session_tokens(&st.db, session_id)
        .await
        .ok();
    crate::runs::delete_for_session(&st.db, session_id).await?;
    Ok(Json(json!({
        "deleted": true,
        "kind": "launch_attempt",
        "warnings": warnings
    })))
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
pub(super) async fn archive_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = match require_session(&st.db, &key).await {
        Ok(found) => found,
        Err(error) if error.is_not_found() => {
            return archive_launch_attempt(&st, &key).await;
        }
        Err(error) => return Err(error),
    };
    tracing::debug!(key = %key, session = %session.id, "handling archive session request");
    let warnings = archive(&st, &session, &branch).await.map_err(|error| {
        // A refusal names a state the caller can act on (another transition owns
        // the session); only a genuine failure becomes the reassuring 500.
        if error.downcast_ref::<crate::lifecycle::Refusal>().is_some() {
            return AppError::from(error);
        }
        AppError::internal(
            format!(
                "Could not finish archiving session {}. Its branch and conversation are safe; retry in a moment.",
                session.id
            ),
            error,
        )
    })?;
    Ok(Json(json!({
        "archived": true,
        "kind": "session",
        "branch": branch.branch,
        "warnings": warnings
    })))
}

/// Archive an unmatched launch attempt: tear down any deterministic reserved
/// runtime and preserve the now-cancelled automation row as history.
async fn archive_launch_attempt(st: &AppState, session_id: &str) -> ApiResult<Json<Value>> {
    if crate::runs::list_for_session(&st.db, session_id)
        .await?
        .is_empty()
    {
        return Err(AppError::not_found("session"));
    }
    crate::runs::cancel_for_session_with_summary(
        &st.db,
        session_id,
        "launch attempt archived by user",
    )
    .await?;
    let warnings = crate::session_manager::teardown_reserved_runtime(session_id).await;
    st.ide.kill(session_id);
    crate::auth::revoke_session_tokens(&st.db, session_id)
        .await
        .ok();
    Ok(Json(json!({
        "archived": true,
        "kind": "launch_attempt",
        "branch": session_id,
        "warnings": warnings
    })))
}

/// `GET /api/sessions/{id}/shells` — the live worktree debug-shell indices for a
/// session, so the UI re-opens the shell tabs after a reload (the shells are
/// detached supervisors that outlive the page). Never spawns.
pub(super) async fn list_session_shells(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<u32>>> {
    let (session, _) = require_session(&st.db, &key).await?;
    Ok(Json(crate::shell::list_debug(&session.id).await))
}

/// `DELETE /api/sessions/{id}/shell/{idx}` — close one worktree debug shell (the
/// tab's ×), killing its supervisor. Idempotent: a missing shell is a no-op.
pub(super) async fn delete_session_shell(
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
/// will pick a freshly-merged PR up within a tick while the session is recent.
pub(super) async fn refresh_github_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    github::refresh(&st, &session, &branch, false)
        .await
        .map_err(|e| github_request_error("refresh this pull request", e))?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct GithubMappingBody {
    pub pr_number: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct GithubLabelsBody {
    pub labels: Vec<String>,
}

/// Add labels to the pull request currently associated with a session. Watch
/// programs use this Loom-owned API instead of receiving a GitHub credential
/// and invoking `gh` themselves.
pub(super) async fn add_github_session_labels(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<GithubLabelsBody>,
) -> ApiResult<Json<Value>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    if req.labels.is_empty() || req.labels.len() > 10 {
        return Err(AppError::bad_request(
            "GitHub labels must contain between 1 and 10 entries",
        ));
    }
    let labels = req
        .labels
        .into_iter()
        .map(|label| label.trim().to_string())
        .collect::<Vec<_>>();
    if labels
        .iter()
        .any(|label| label.is_empty() || label.len() > 100)
    {
        return Err(AppError::bad_request(
            "each GitHub label must contain between 1 and 100 bytes",
        ));
    }
    let status = github::get_status(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::bad_request("session has no associated pull request"))?;
    let repo_root = PathBuf::from(&branch.repo_root);
    let slug = crate::repo::github_slug_for_root(&st.db, &repo_root)
        .await?
        .ok_or_else(|| AppError::bad_request("session repository has no GitHub identity"))?;
    let repo = crate::repo::parse_slug(&slug)
        .map_err(|_| AppError::bad_request("session GitHub repository is invalid"))?;
    let app = super::configured_github_app(&st).await?;
    app.add_thread_labels(&repo, status.pr_number, &labels)
        .await
        .map_err(|e| github_request_error("label this pull request", e))?;
    Ok(Json(json!({
        "number": status.pr_number,
        "labels": labels,
    })))
}

/// Pin a session's branch to an explicit PR and fetch that PR immediately. The
/// mapping is persisted only after GitHub confirms the number, so a typo never
/// replaces a working association with a dead one.
pub(super) async fn set_github_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<GithubMappingBody>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    if req.pr_number <= 0 {
        return Err(AppError::bad_request("PR number must be positive"));
    }
    let repo_root = PathBuf::from(&branch.repo_root);
    let snap = github::fetch_pr(&st, &repo_root, req.pr_number)
        .await
        .map_err(|e| github_request_error("find this pull request", e))?
        .ok_or_else(|| {
            AppError::bad_request(format!("pull request #{} was not found", req.pr_number))
        })?;
    github::set_mapping(&st.db, &branch.id, req.pr_number).await?;
    github::apply_snapshot(&st, &session, &branch, &snap, false).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

fn github_request_error(action: &str, error: anyhow::Error) -> AppError {
    tracing::warn!(%error, action, "GitHub request failed");
    AppError::new(
        StatusCode::BAD_GATEWAY,
        format!("Loom couldn't {action} on GitHub. Check Settings > Access or try again."),
    )
}

/// Clear an explicit PR mapping and return to automatic current-open-PR
/// discovery. The cached snapshot is cleared first so an old open PR cannot
/// pull auto mode back to itself on the next refresh.
pub(super) async fn clear_github_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    github::clear_mapping(&st.db, &branch.id).await?;
    github::clear_status(&st.db, &branch.id).await?;
    if let Err(e) = github::refresh(&st, &session, &branch, false).await {
        tracing::debug!(branch = %branch.branch, error = %e, "automatic PR refresh after clearing mapping failed");
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

pub(super) async fn adopt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    tracing::debug!(key = %key, session = %session.id, "handling adopt session request");
    adopt(&st, &session, &branch).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Recover an archived session: rebuild its worktree from the kept branch, then
/// resume the agent — the inverse of [`archive`]. Where archive tears the worktree
/// down but keeps the branch (and its commits), the session row, and the history,
/// recover checks that branch back out at the same worktree path and re-launches
/// the agent (resuming the prior Claude conversation with `--continue`, exactly as
/// [`adopt`] does). The session rejoins the active fleet.
async fn recover(st: &AppState, session: &Session, _branch: &Branch) -> Result<(), AppError> {
    let _source_permit = st.launch_gate.acquire_session(&session.id).await;
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    let session = &current_session;
    let branch = &current_branch;
    tracing::info!(session = %session.id, branch = %branch.id, "recovering archived session");
    let refreshed = crate::lifecycle::release_abandoned_transition(&st.db, session).await?;
    let session = &refreshed;
    require_no_transition(session)?;
    if session.status != "archived" {
        return Err(AppError::conflict(format!(
            "session is '{}', not archived",
            session.status
        )));
    }
    let _profile_permit = st.launch_gate.acquire_profile(&session.profile).await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    let session = &current_session;
    let branch = &current_branch;
    if session.status != "archived" {
        return Err(AppError::conflict(format!(
            "session is '{}', not archived",
            session.status
        )));
    }
    let profile = require_session_profile_lifetime(&st.db, session).await?;
    require_resume_capacity(&st.db, session, &profile).await?;
    let custom_agent = stamped_custom_agent(session)?;
    require_branch_slot_free(st, session, branch).await?;
    // Reserve the active branch slot in SQLite before touching the worktree or
    // supervisor. This is the atomic boundary a read-then-launch guard cannot
    // provide: a concurrent new session either owns the unique slot or this
    // recovery does, never both after external state has been created.
    match session_mod::claim_recovery(&st.db, &session.id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::conflict(
                "session is no longer archived — another lifecycle action won",
            ))
        }
        Err(error) => {
            if let Some(other) = session_mod::active_for_branch(&st.db, &branch.id).await? {
                if other.id != session.id {
                    return Err(AppError::conflict(format!(
                        "branch '{}' already has an active session ({})",
                        branch.branch, other.id
                    )));
                }
            }
            return Err(error.into());
        }
    }
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    let mut rebuilt_worktree = false;

    let result: Result<(), AppError> = async {
        // Rebuild the worktree if archive removed it. Archive keeps the branch,
        // but a later manual `git branch -D` could have deleted it — refuse
        // clearly rather than let the checkout fail cryptically.
        if !work_dir.exists() {
            crate::lifecycle::transition_step(
                st,
                session,
                branch,
                "adopting",
                "Rebuilding worktree",
            )
            .await?;
            if !git::branch_exists(&repo_root, &branch.branch).await {
                return Err(AppError::bad_request(format!(
                    "branch '{}' no longer exists — cannot recover",
                    branch.branch
                )));
            }
            // Clear any stale worktree registration at this path first:
            // archive's forced remove deregisters, but a manual `rm -rf` of the
            // dir would leave git's admin entry behind and reject re-adding it.
            git::worktree_prune(&repo_root).await.ok();
            tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
            git::ensure_excluded(&repo_root, ".worktrees/")
                .await
                .ok();
            tracing::info!(session = %session.id, branch = %branch.id, work_dir = %work_dir.display(), "rebuilding worktree for recovered session");
            // Mark this attempt as the owner before invoking git: a failed
            // `worktree add` may still have created the directory or registry
            // entry, both of which belong in this attempt's rollback.
            rebuilt_worktree = true;
            git::worktree_add_existing(&repo_root, &work_dir, &branch.branch)
                .await
                .map_err(|e| AppError::bad_request(e.to_string()))?;
        } else {
            tracing::debug!(session = %session.id, "worktree still present, skipping rebuild");
        }

        crate::lifecycle::transition_step(st, session, branch, "adopting", "Resuming agent")
            .await?;
        tracing::debug!(session = %session.id, branch = %branch.id, protocol = %session.protocol, "resuming recovered agent");
        if session.protocol == "acp" {
            Ok(adopt_acp(
                st,
                session,
                branch,
                "session recovered",
                custom_agent.as_ref(),
            )
            .await?)
        } else if backend::has_session(&session.term_session).await {
            // Repair an old partial archive without killing the agent that is
            // still doing useful work. New archives cannot create this state:
            // they wait for the supervisor to disappear before writing
            // `archived`.
            let status = agent::initial_status(&st.db, &session.agent_kind).await;
            session_mod::set_status(&st.db, &session.id, status).await?;
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "status",
                json!({ "status": status, "reason": "session recovered" }),
            )
            .await
            .ok();
            Ok(())
        } else {
            Ok(resume_agent(
                st,
                session,
                branch,
                "session recovered",
                custom_agent.as_ref(),
            )
            .await?)
        }
    }
    .await;

    if let Err(error) = result {
        // Recovery is all-or-nothing. Tear down anything this attempt launched
        // before restoring `archived`; if teardown itself fails, keep the
        // non-terminal reservation rather than recreating the forbidden
        // archived+live-supervisor state.
        st.acp.stop(&session.id);
        if let Err(cleanup) = backend::kill_session_and_wait(&session.term_session).await {
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "recovery failed: {}; cleanup also failed: {cleanup}",
                    error.message()
                ),
            ));
        }
        if rebuilt_worktree {
            if work_dir.exists() && git::worktree_remove(&repo_root, &work_dir).await.is_err() {
                tokio::fs::remove_dir_all(&work_dir).await.ok();
            }
            // Also clear a registry entry left by a partially successful
            // `worktree add` whose directory never became visible.
            git::worktree_prune(&repo_root).await.ok();
        }
        session_mod::complete_transition(&st.db, &session.id, "adopting", "archived").await?;
        return Err(error);
    }

    crate::lifecycle::transition_step(st, session, branch, "adopting", "Finalizing recovery")
        .await?;
    let completed_status = session_mod::get(&st.db, &session.id)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?
        .status;
    if !session_mod::complete_transition(&st.db, &session.id, "adopting", &completed_status).await?
    {
        return Err(AppError::conflict(
            "recovery lost ownership of its lifecycle transition",
        ));
    }

    crate::channels::reopen_session_channel(&st.db, &session.id).await?;

    // Stamp the durable opt-out from immediate merge re-archive only after the
    // live side committed. A bookkeeping failure must not roll back a healthy
    // recovered agent.
    match tags::set(
        &st.db,
        &branch.id,
        tags::RECOVERED_KEY,
        tags::RECOVERED_VALUE,
        "session recovered",
        "loom",
    )
    .await
    {
        Ok(()) => {
            events::record_tag(
                &st.db,
                &st.bus,
                &branch.id,
                tags::RECOVERED_KEY,
                tags::RECOVERED_VALUE,
                "session recovered",
                "loom",
            )
            .await
            .ok();
        }
        Err(error) => {
            tracing::warn!(session = %session.id, %error, "could not stamp recovered tag");
        }
    }
    Ok(())
}

pub(super) async fn recover_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<SessionView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    tracing::debug!(key = %key, session = %session.id, "handling recover session request");
    if session.status == "archived" {
        recover(&st, &session, &branch).await?;
    } else {
        crate::lifecycle::recover_acp_runtime(&st, &session).await?;
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

// ---------------------------------------------------------------------------
// Raw worktree bytes — serves a single file's bytes (with a guessed content
// type) for Markdown inline images. The embedded editor ([`crate::ide`]) is the
// file browsing/editing surface; this endpoint only reads, never writes.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct RawQuery {
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
pub(super) async fn raw_session(
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

// Branch history compatibility alias plus session conversation/event endpoints.

pub(super) async fn branch_events(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Event>>> {
    let branch = require_branch(&st.db, &key).await?;
    Ok(Json(events::history(&st.db, &branch.id, 200).await?))
}

/// The session's agent conversation as a normalized iris log — the live
/// transcript when present, else the capture archived alongside it. 404 when the
/// session has no conversation (e.g. a `shell` session, or none recorded yet).
pub(super) async fn conversation_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Response> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let mut log = crate::chatlog::conversation(&st.db, &session, &branch)
        .await
        .ok_or_else(|| AppError::not_found("conversation"))?;
    // Oversized tool payloads are served as previews pointing at their own URL
    // (see `elide_tool_payloads`), so this response stays bounded by the number
    // of blocks rather than by how much output the agent's tools happened to
    // produce. `conversation_block` serves any one of them in full.
    let session_id = session.id.clone();
    // A terminal transcript can be many megabytes. JSON serialization is CPU
    // work too, so keep it beside discovery/parsing on the blocking pool rather
    // than letting a large response stall unrelated async routes.
    let body = tokio::task::spawn_blocking(move || {
        crate::chatlog::elide_tool_payloads(&mut log, &session_id);
        serde_json::to_vec(&log)
    })
    .await??;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// One conversation block, untruncated — the target of the `full` pointer that
/// [`conversation_session`] leaves in place of an oversized tool payload.
/// Addressed by position in the log; see `elide_tool_payloads` for why.
pub(super) async fn conversation_block(
    State(st): State<AppState>,
    Path((key, message, block)): Path<(String, usize, usize)>,
) -> ApiResult<Response> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let log = crate::chatlog::conversation(&st.db, &session, &branch)
        .await
        .ok_or_else(|| AppError::not_found("conversation"))?;
    let found = log
        .messages
        .get(message)
        .and_then(|m| m.blocks.get(block))
        .ok_or_else(|| AppError::not_found("conversation block"))?;
    let body = serde_json::to_vec(found)?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct HistoryPageQuery {
    before: Option<String>,
    limit: Option<usize>,
    /// Comma-separated normalized record kinds.
    kinds: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct HistorySearchQuery {
    q: String,
    before: Option<String>,
    limit: Option<usize>,
    /// Comma-separated normalized record kinds.
    kinds: Option<String>,
}

fn history_kinds(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn history_error(error: crate::history::PageError) -> AppError {
    match error {
        crate::history::PageError::BadRequest(message) => AppError::bad_request(message),
        crate::history::PageError::Internal(error) => error.into(),
    }
}

/// A provider-neutral page of this session's conversation records. ACP reads
/// its durable journal; terminal sessions normalize their native transcript on
/// read and fall back to the archived Iris capture.
pub(super) async fn session_history(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<HistoryPageQuery>,
) -> ApiResult<Json<HistoryPageView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let page = crate::history::page(
        &st.db,
        &session,
        &branch,
        crate::history::PageOptions {
            before: query.before,
            limit: query.limit,
            kinds: history_kinds(query.kinds.as_deref()),
            query: None,
        },
    )
    .await
    .map_err(history_error)?;
    Ok(Json(page))
}

/// Case-insensitive literal search over the same normalized, session-scoped
/// records and cursor contract as [`session_history`].
pub(super) async fn search_session_history(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<HistorySearchQuery>,
) -> ApiResult<Json<HistoryPageView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let page = crate::history::page(
        &st.db,
        &session,
        &branch,
        crate::history::PageOptions {
            before: query.before,
            limit: query.limit,
            kinds: history_kinds(query.kinds.as_deref()),
            query: Some(query.q),
        },
    )
    .await
    .map_err(history_error)?;
    Ok(Json(page))
}

/// The `sessions.events.stream` operation — one session's event feed as it
/// happens. The durable half is `sessions.events.list`.
pub(super) async fn events_sse(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<ops::events::stream::Input>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let input =
        super::streams::authorized::<ops::events::stream::Stream>(&st, &principal, input).await?;
    let branch = require_branch(&st.db, &input.session).await?;
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
/// `by` (a watch name, or `manual` when absent).
pub(super) async fn send_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    // A cross-session send must not sit behind an ACP turn indefinitely or be
    // discarded with an adapter's in-memory steer. Cancel a live turn and
    // immediately start this message as a fresh turn, keeping the same audit.
    if session.protocol == "acp" {
        let handle = require_acp_task(&st, &session)?;
        let by = author_or_manual(req.by.as_deref());
        let ack = handle
            .stop_and_send(req.text.clone(), Some(by.clone()), Vec::new())
            .await
            .map_err(|e| AppError::conflict(e.to_string()))?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "nudge",
            json!({ "by": by, "text": req.text }),
        )
        .await
        .ok();
        return Ok(Json(json!({
            "sent": true,
            "submitted": true,
            "queued": ack.queued,
            "turn": ack.turn,
        })));
    }
    require_live_terminal(&session).await?;
    backend::paste(&session.term_session, &req.text)
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

/// Send a break/interrupt to a session. For an ACP session this is a
/// `session/cancel` notification (the turn still ends via its prompt response,
/// stop reason `cancelled`); for a terminal session it is `Escape`, the keystroke
/// Claude Code reads as "stop the current turn".
pub(super) async fn interrupt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    if session.protocol == "acp" {
        let handle = require_acp_task(&st, &session)?;
        handle
            .cancel()
            .await
            .map_err(|e| AppError::conflict(e.to_string()))?;
        return Ok(Json(json!({ "interrupted": true })));
    }
    require_live_terminal(&session).await?;
    backend::send_key(&session.term_session, "Escape")
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "interrupted": true })))
}

#[derive(Debug, Deserialize)]
pub(super) struct PreviewQuery {
    /// Extra scrollback lines to include above the visible screen (0 = just the
    /// visible pane).
    #[serde(default)]
    lines: usize,
}

/// Capture the session's terminal pane as plain text — "what does the child look
/// like right now". Returns `{ "screen": "<text>" }`.
pub(super) async fn preview_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<PreviewQuery>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    // An ACP session has no vt100 screen; its `preview` is the last N journal
    // blocks rendered as plain text (CLI convenience). `lines` is the block count,
    // defaulting to a reasonable tail when unset.
    if session.protocol == "acp" {
        let n = if q.lines == 0 { 40 } else { q.lines };
        // Only the tail is rendered, so only the tail is read — a session that has
        // been running for days has a journal far larger than any preview.
        let (blocks, _) = crate::chat::list_page(&st.db, &session.id, None, n).await?;
        let screen = crate::chat::preview_text(&blocks, n);
        return Ok(Json(json!({ "screen": screen })));
    }
    require_live_terminal(&session).await?;
    let screen = backend::capture(&session.term_session, q.lines)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "screen": screen })))
}

// ---------------------------------------------------------------------------
// The ACP chat journal + drive routes (protocol='acp' sessions)
//
// The conversation-first surface for ACP sessions: the journaled transcript
// (`/chat`), its live delta stream (`/chat/stream`), and the drive routes a
// person or watch uses — a durable queueing send (`/prompt`), a
// permission answer (`/permissions/{request_id}`), and a mode change (`/mode`).
// ---------------------------------------------------------------------------

/// Guard: the route only applies to an ACP session; a terminal session 409s (it
/// has no chat journal — its transcript is the JSONL scrape at `/conversation`).
fn require_acp(session: &Session) -> ApiResult<()> {
    if session.protocol == "acp" {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "session '{}' is a terminal session, not an ACP conversation",
            session.id
        )))
    }
}

/// The live ACP task handle for a session, or 409 when no task is running (the
/// session is idle/orphaned — nothing to drive over the protocol right now).
fn require_acp_task(st: &AppState, session: &Session) -> ApiResult<crate::acp::AcpHandle> {
    st.acp.get(&session.id).ok_or_else(|| {
        AppError::conflict(format!(
            "session '{}' has no live ACP task to drive",
            session.id
        ))
    })
}

/// Replace the provider behind an idle ACP work session while preserving Loom's
/// stable session/branch/worktree identity and canonical journal.
pub(super) async fn handoff_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<HandoffReq>,
) -> ApiResult<Json<SessionView>> {
    let (initial_session, _) = require_session(&st.db, &key).await?;
    let (session, branch) = crate::handoff::handoff_session(&st, initial_session, req)
        .await
        .map_err(map_handoff_error)?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}
/// Permission posture captured when the persisted in-flight turn started. This
/// differs from `Session.current_mode` after a live config change: that selection
/// applies to the next prompt.
fn effective_turn_mode(session: &Session) -> Option<String> {
    session
        .acp_inflight
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("mode").and_then(Value::as_str).map(str::to_string))
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatPageQuery {
    before_turn: Option<i64>,
    before_seq: Option<i64>,
}

const CHAT_PAGE_SIZE: usize = 200;

/// The journaled conversation plus the agent-owned composer metadata. The
/// journal works without a live task; metadata is empty until an adapter is
/// attached and advertises its commands/configuration controls.
pub(super) async fn get_session_chat(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<ChatPageQuery>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let before = match (query.before_turn, query.before_seq) {
        (Some(turn), Some(seq)) => Some((turn, seq)),
        (None, None) => None,
        _ => {
            return Err(AppError::bad_request(
                "before_turn and before_seq must be supplied together",
            ))
        }
    };
    let (blocks, has_more) =
        crate::chat::list_page(&st.db, &session.id, before, CHAT_PAGE_SIZE).await?;
    let older_cursor = if has_more {
        blocks
            .first()
            .map(|block| json!({ "turn": block.turn, "seq": block.seq }))
    } else {
        None
    };
    let metadata = match st.acp.get(&session.id) {
        Some(handle) => handle.metadata(),
        None => match session_mod::get_acp_metadata(&st.db, &session.id).await? {
            Some(raw) => serde_json::from_str(&raw)?,
            None => crate::acp::AcpMetadata::default(),
        },
    };
    // Storage uses '' for compatibility with long-lived NOT NULL databases;
    // keep that sentinel out of the public conversation contract.
    let pending_prompt = session
        .pending_prompt
        .as_deref()
        .filter(|pending| !pending.trim().is_empty());
    Ok(Json(json!({
        "blocks": blocks,
        "older_cursor": older_cursor,
        "live_turn": session_mod::acp_inflight_turn(&session),
        "effective_mode": effective_turn_mode(&session),
        "pending_prompt": pending_prompt,
        "metadata": metadata,
    })))
}

/// The live SSE tail of the conversation — `block` / `delta` / `tool` / `turn`
/// events (see [`crate::acp`]), plus `resync` when the bounded broadcast drops
/// frames. A client reads `sessions.chat` first, then applies this tail. When no
/// task is running the stream stays open but silent (keep-alive).
pub(super) async fn chat_stream(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<ops::chat::stream::Input>,
) -> ApiResult<impl IntoResponse> {
    let input =
        super::streams::authorized::<ops::chat::stream::Stream>(&st, &principal, input).await?;
    let (session, _) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let boxed: Pin<Box<dyn Stream<Item = Result<sse::Event, Infallible>> + Send>> =
        match st.acp.get(&session.id) {
            Some(handle) => {
                let stream = BroadcastStream::new(handle.subscribe()).map(|result| {
                    let (event, data) = super::eventmux::chat_event_parts(result);
                    Ok(sse::Event::default()
                        .event(event)
                        .json_data(data)
                        .unwrap_or_default())
                });
                Box::pin(stream)
            }
            // No live task: hold the connection open (keep-alive) with no events.
            None => Box::pin(tokio_stream::pending()),
        };
    Ok(Sse::new(boxed).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
pub(super) struct PromptBody {
    pub text: String,
    #[serde(default)]
    pub by: Option<String>,
    /// Deliver this user message immediately by cancelling any live turn and
    /// starting the message as a normal prompt.
    #[serde(default)]
    pub send_now: bool,
    /// Promote the server's durable next-turn queue instead of sending `text`.
    /// This keeps the action race-free when the browser is showing queued copy.
    #[serde(default)]
    pub force_queued: bool,
    /// Worktree-relative files selected by the composer. The server resolves
    /// and validates them, then forwards ACP resource-link blocks.
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FileSearchQuery {
    #[serde(default)]
    q: String,
}

/// Server-side worktree file completion for the chat composer. The browser has
/// no filesystem access; git supplies tracked plus unignored untracked files.
pub(super) async fn list_session_files(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<FileSearchQuery>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let out = tokio::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(&session.work_dir)
        .output()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !out.status.success() {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            format!(
                "git ls-files failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    let needle = query.q.trim().to_ascii_lowercase();
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| String::from_utf8(raw.to_vec()).ok())
        .filter(|path| needle.is_empty() || path.to_ascii_lowercase().contains(&needle))
        .collect();
    files.sort_by_key(|path| {
        let lower = path.to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        (
            !lower.starts_with(&needle),
            !name.starts_with(&needle),
            path.len(),
            lower,
        )
    });
    files.truncate(40);
    Ok(Json(json!({ "files": files })))
}

async fn prompt_resources(work_dir: &str, files: &[String]) -> ApiResult<Vec<Value>> {
    use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
    const FILE_URI_ENCODE: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}');

    let root = tokio::fs::canonicalize(work_dir).await?;
    let mut out = Vec::new();
    for requested in files {
        let relative = std::path::Path::new(requested);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::RootDir))
        {
            return Err(AppError::bad_request(format!(
                "invalid file reference '{requested}'"
            )));
        }
        let canonical = tokio::fs::canonicalize(root.join(relative))
            .await
            .map_err(|_| AppError::bad_request(format!("file '{requested}' does not exist")))?;
        if !canonical.starts_with(&root) || !canonical.is_file() {
            return Err(AppError::bad_request(format!(
                "file reference '{requested}' is outside the worktree"
            )));
        }
        let uri = format!(
            "file://{}",
            utf8_percent_encode(&canonical.to_string_lossy(), FILE_URI_ENCODE)
        );
        out.push(json!({
            "type": "resource_link",
            "name": requested,
            "uri": uri,
        }));
    }
    Ok(out)
}

/// Send a user message to an ACP session. A normal request is dispatched when
/// idle or appended to the durable queue while a turn is live; `send_now`
/// instead cancels any live turn and starts the message as a normal prompt.
/// Returns 202 `{ queued, turn }`. Every send records a `nudge` event (the audit
/// rule).
pub(super) async fn prompt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PromptBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (session, branch) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let handle = require_acp_task(&st, &session)?;
    let by = author_or_manual(req.by.as_deref());
    let audit_text = if req.force_queued {
        session_mod::read_pending_prompt(&st.db, &session.id).await?
    } else {
        req.text.clone()
    };
    let ack = if req.force_queued {
        handle.force_pending(Some(by.clone())).await
    } else {
        let resources = prompt_resources(&session.work_dir, &req.files).await?;
        if req.send_now {
            handle
                .stop_and_send(req.text.clone(), Some(by.clone()), resources)
                .await
        } else {
            handle
                .prompt(req.text.clone(), Some(by.clone()), resources)
                .await
        }
    }
    .map_err(|e| AppError::conflict(e.to_string()))?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({
            "by": by,
            "text": audit_text,
            "send_now": req.send_now,
            "promoted_queue": req.force_queued,
        }),
    )
    .await
    .ok();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "queued": ack.queued,
            "turn": ack.turn,
        })),
    ))
}

/// Pull unseen next-turn feedback back out of the durable queue for editing.
/// The ACP task owns the consume so this action is serialized with automatic
/// dispatch at a turn boundary.
pub(super) async fn retract_queued_prompt(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let handle = require_acp_task(&st, &session)?;
    let text = handle
        .retract_pending()
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(Json(json!({ "text": text })))
}

#[derive(Debug, Deserialize)]
pub(super) struct ConfigOptionBody {
    pub value: Value,
}

/// Change one agent-owned session configuration selector. This waits for the
/// adapter's response, whose full refreshed option list is broadcast to chat
/// clients as a `metadata` event.
pub(super) async fn set_config_option(
    State(st): State<AppState>,
    Path((key, config_id)): Path<(String, String)>,
    Json(req): Json<ConfigOptionBody>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let handle = require_acp_task(&st, &session)?;
    let metadata = handle
        .set_config_option(config_id.clone(), req.value.clone())
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(Json(json!({
        "config_id": config_id,
        "value": req.value,
        "metadata": metadata,
    })))
}

#[derive(Debug, Deserialize)]
pub(super) struct PermissionBody {
    pub option_id: String,
    #[serde(default)]
    pub by: Option<String>,
}

/// Answer a pending permission request: 200 on success, 404 for an unknown
/// request id, 409 when it was already resolved.
pub(super) async fn answer_permission(
    State(st): State<AppState>,
    Extension(principal): Extension<crate::auth::Principal>,
    Path((key, request_id)): Path<(String, String)>,
    Json(req): Json<PermissionBody>,
) -> ApiResult<Json<Value>> {
    if !principal.is_human() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "agent permission prompts require a human operator decision",
        ));
    }
    let (session, branch) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let handle = require_acp_task(&st, &session)?;
    let by = author_or_manual(req.by.as_deref());
    match handle
        .answer_permission(request_id.clone(), req.option_id.clone(), by.clone())
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?
    {
        crate::acp::PermAnswer::Ok => {
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "permission",
                json!({ "by": by, "request_id": request_id, "option_id": req.option_id }),
            )
            .await
            .ok();
            Ok(Json(
                json!({ "resolved": true, "option_id": req.option_id }),
            ))
        }
        crate::acp::PermAnswer::NotFound => Err(AppError::not_found("permission request")),
        crate::acp::PermAnswer::AlreadyResolved => {
            Err(AppError::conflict("permission request already resolved"))
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ModeBody {
    pub mode_id: String,
    #[serde(default)]
    pub by: Option<String>,
}

/// Change an ACP session's mode (`session/set_mode`), journaling a `mode_change`
/// block. Returns `{ mode_id }`.
pub(super) async fn set_mode(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<ModeBody>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    if session.policy_restricted && req.mode_id != "default" {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "restricted sessions cannot change permission mode",
        ));
    }
    let handle = require_acp_task(&st, &session)?;
    let by = author_or_manual(req.by.as_deref());
    handle
        .set_mode(req.mode_id.clone(), Some(by.clone()))
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({ "by": by, "mode": req.mode_id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "mode_id": req.mode_id })))
}

// ---------------------------------------------------------------------------
// Operation registry bindings
//
// The `sessions` bundle: 17 operations under `weaver_api::operations::sessions`.
// `sessions.summary.get` lives in the sibling `session_summary.rs` and is
// folded in below. `sessions.context` also lives in a sibling file
// (`self_context.rs`), but `registry()` (`web/operations.rs`, coordinator-owned)
// already wires `self_context::bound_operations()` in on its own — it predates
// `context` moving into this bundle and still treats it as independent — so it
// is deliberately NOT re-added here; doing so would register `sessions.context`
// twice and panic the router on startup. Every other handler lives here,
// beside the legacy axum handler it was ported from (which stays mounted —
// see `web/mod.rs` — until the coordinator removes it in one pass).
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    let mut bound = vec![
        register::<ops::list::List, _, _>(op_list),
        register::<ops::get::Get, _, _>(op_get),
        register::<ops::launch::Launch, _, _>(op_launch),
        register::<ops::launches::resolve::Resolve, _, _>(op_launches_resolve),
        register::<ops::send::Send, _, _>(op_send),
        register::<ops::interrupt::Interrupt, _, _>(op_interrupt),
        register::<ops::preview::Preview, _, _>(op_preview),
        register::<ops::events::list::List, _, _>(op_events_list),
        register::<ops::events::create::Create, _, _>(op_events_create),
        register::<ops::history::list::List, _, _>(op_history_list),
        register::<ops::history::search::Search, _, _>(op_history_search),
        register::<ops::status::get::Get, _, _>(op_status_get),
        register::<ops::status::set::Set, _, _>(op_status_set),
        register::<ops::tags::list::List, _, _>(op_tags_list),
        register::<ops::tags::set::Set, _, _>(op_tags_set),
        register::<ops::tags::delete::Delete, _, _>(op_tags_delete),
        register::<ops::adopt::Adopt, _, _>(op_adopt),
        register::<ops::archive::Archive, _, _>(op_archive),
        register::<ops::recover::Recover, _, _>(op_recover),
        register::<ops::handoff::Handoff, _, _>(op_handoff),
        register::<ops::chat::Chat, _, _>(op_chat),
        register::<ops::conversation::Conversation, _, _>(op_conversation),
        register::<ops::files::Files, _, _>(op_files),
        register::<ops::mode::Mode, _, _>(op_mode),
        register::<ops::raw::Raw, _, _>(op_raw),
        register::<ops::url::Url, _, _>(op_url),
        register::<ops::ide_info::IdeInfo, _, _>(op_ide_info),
        register::<ops::shells::list::List, _, _>(op_shells_list),
        register::<ops::shells::delete::Delete, _, _>(op_shells_delete),
        register::<ops::update::Update, _, _>(op_update),
        register::<ops::delete::Delete, _, _>(op_delete),
        register::<ops::config::set::Set, _, _>(op_config_set),
        register::<ops::conversation::block::Block, _, _>(op_conversation_block),
        register::<ops::github::refresh::Refresh, _, _>(op_github_refresh),
        register::<ops::github::set::Set, _, _>(op_github_set),
        register::<ops::github::clear::Clear, _, _>(op_github_clear),
        register::<ops::github::labels::add::Add, _, _>(op_github_labels_add),
        register::<ops::handoff::resolve::Resolve, _, _>(op_handoff_resolve),
        register::<ops::prompt::create::Create, _, _>(op_prompt_create),
        register::<ops::prompt::retract::Retract, _, _>(op_prompt_retract),
        register::<ops::resumption_cue::get::Get, _, _>(op_resumption_cue_get),
        register::<ops::resumption_cue::ensure::Ensure, _, _>(op_resumption_cue_ensure),
        register::<ops::permissions::answer::Answer, _, _>(op_permissions_answer),
        register::<ops::title::regenerate::Regenerate, _, _>(op_title_regenerate),
        register::<ops::title::generation::set::Set, _, _>(op_title_generation_set),
    ];
    bound.extend(super::session_summary::bound_operations());
    bound.extend(super::changes::bound_operations());
    bound.extend(super::scratch::bound_operations());
    bound
}

/// `sessions.list` — ported from [`search_sessions`], which this replaces:
/// the operation's `Input` is exactly `SearchSessionsOptions`'s field set.
async fn op_list(
    context: OperationContext,
    input: ops::list::Input,
) -> ApiResult<Vec<SessionView>> {
    collect_sessions(
        &context.state,
        false,
        SessionCollectionFilter {
            archived: input.history || input.archived_only,
            archived_only: input.archived_only,
            automation: true,
            search: Some(&input.q),
            status: input.status,
            attention: input.attention,
            creator: input.creator,
            viewer: &context.principal.username,
        },
    )
    .await
}

/// `sessions.get` — ported from [`get_session`].
async fn op_get(context: OperationContext, input: ops::get::Input) -> ApiResult<SessionView> {
    let (session, branch) = require_session(&context.state.db, &input.session).await?;
    session_view(&context.state.db, &session, &branch).await
}

/// `sessions.launch` — ported from [`create_session`]. The operation's surface
/// is a reduced `CreateReq`: fields `create_session` accepts but this
/// operation doesn't declare (`model`, `effort`, `scratch`, `protocol`,
/// `mode`, `class`, `name`, `github_issue`, `existing_branch`, the canonical
/// `selection`/revision pair) are left at `CreateReq::default()`, identical to
/// what a client posting a body that omits them already gets today.
///
/// The old handler's inline rejection of `Grant::Automation` is dropped: this
/// operation declares `actor = SessionSelf`, so `authorize()` now refuses an
/// automation credential before this handler ever runs. Likewise, the old
/// handler overwrote `req.parent_branch` from the principal for a
/// `Grant::Session` caller — `parent_branch` is `#[operand(context = "branch")]`
/// here, so the dispatcher has already resolved it the same way (the caller's
/// own branch, or unset for a human/admin launch) before this handler sees it.
async fn op_launch(context: OperationContext, input: ops::launch::Input) -> ApiResult<SessionView> {
    let st = context.state;
    let req = CreateReq {
        title: Some(input.title),
        goal: input.goal,
        repo: input.repo,
        cwd: input.cwd,
        base: input.base,
        agent: input.agent,
        profile: input.profile,
        claim_issue: input.claim_issue,
        issue: input.issue,
        parent_branch: input.parent_branch,
        name: input.name,
        existing_branch: input.existing_branch,
        github_issue: input.github_issue,
        model: input.model,
        effort: input.effort,
        selection: input.selection,
        scratch: input.scratch,
        expected_profile_revision: input.expected_profile_revision,
        expected_resolver_revision: input.expected_resolver_revision,
        ..Default::default()
    };
    if let Some(repo_input) = req.repo.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        ensure_repo_registered(&st.db, repo_input).await?;
    }
    let delegated = req
        .parent_branch
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    // `authorize()` has already refused an anonymous or automation grant for
    // this `actor = SessionSelf` operation, so `from_principal` cannot
    // actually return `None` here; fail closed rather than depend on that.
    let actor = crate::provision::Actor::from_principal(&context.principal, delegated).ok_or_else(
        || AppError::new(StatusCode::FORBIDDEN, "credential cannot launch a session"),
    )?;
    let created = crate::provision::create(st.clone(), req, actor)
        .await
        .map_err(super::provision_error)?;
    session_view(&st.db, &created.session, &created.branch).await
}

/// `sessions.send` — ported from [`send_session`].
async fn op_send(
    context: OperationContext,
    input: ops::send::Input,
) -> ApiResult<SessionSendResult> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    let submit = input.submit.unwrap_or(true);
    if session.protocol == "acp" {
        let handle = require_acp_task(st, &session)?;
        let by = author_or_manual(input.by.as_deref());
        let ack = handle
            .stop_and_send(input.text.clone(), Some(by.clone()), Vec::new())
            .await
            .map_err(|e| AppError::conflict(e.to_string()))?;
        events::record(
            &st.db,
            &st.bus,
            &branch.id,
            "nudge",
            json!({ "by": by, "text": input.text }),
        )
        .await
        .ok();
        return Ok(SessionSendResult {
            sent: true,
            submitted: true,
            queued: Some(ack.queued),
            turn: ack.turn,
        });
    }
    require_live_terminal(&session).await?;
    backend::paste(&session.term_session, &input.text)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if submit {
        backend::send_enter(&session.term_session)
            .await
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let by = author_or_manual(input.by.as_deref());
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({ "by": by, "text": input.text }),
    )
    .await
    .ok();
    Ok(SessionSendResult {
        sent: true,
        submitted: submit,
        queued: None,
        turn: None,
    })
}

/// `sessions.interrupt` — ported from [`interrupt_session`].
async fn op_interrupt(
    context: OperationContext,
    input: ops::interrupt::Input,
) -> ApiResult<SessionInterruptResult> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    if session.protocol == "acp" {
        let handle = require_acp_task(st, &session)?;
        handle
            .cancel()
            .await
            .map_err(|e| AppError::conflict(e.to_string()))?;
        return Ok(SessionInterruptResult { interrupted: true });
    }
    require_live_terminal(&session).await?;
    backend::send_key(&session.term_session, "Escape")
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(SessionInterruptResult { interrupted: true })
}

/// `sessions.preview` — ported from [`preview_session`].
async fn op_preview(
    context: OperationContext,
    input: ops::preview::Input,
) -> ApiResult<SessionPreviewResult> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    let lines = usize::try_from(input.lines).unwrap_or(0);
    if session.protocol == "acp" {
        let n = if lines == 0 { 40 } else { lines };
        let (blocks, _) = crate::chat::list_page(&st.db, &session.id, None, n).await?;
        let screen = crate::chat::preview_text(&blocks, n);
        return Ok(SessionPreviewResult { screen });
    }
    require_live_terminal(&session).await?;
    let screen = backend::capture(&session.term_session, lines)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(SessionPreviewResult { screen })
}

/// `sessions.events.list` — ported from [`branch_events`] (also mounted at
/// `GET /sessions/{id}/log`, session-key-resolving already via
/// `require_branch`), scoped here to the session directly.
async fn op_events_list(
    context: OperationContext,
    input: ops::events::list::Input,
) -> ApiResult<Vec<Event>> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    Ok(events::history(&st.db, &branch.id, 200).await?)
}

/// `sessions.events.create` — the session-scoped counterpart of
/// [`create_branch_event`](super::branches) (branches.rs, not ported here):
/// same escape-hatch semantics, resolved from a session id instead of a
/// branch key.
async fn op_events_create(
    context: OperationContext,
    input: ops::events::create::Input,
) -> ApiResult<Event> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    let kind = input.kind.trim();
    if kind.is_empty() {
        return Err(AppError::bad_request("event kind is required"));
    }
    let event = events::record(&st.db, &st.bus, &branch.id, kind, input.data).await?;
    tracing::info!(branch = %branch.id, kind = %kind, "session event created");
    Ok(event)
}

/// `sessions.history.list` — ported from [`session_history`].
async fn op_history_list(
    context: OperationContext,
    input: ops::history::list::Input,
) -> ApiResult<HistoryPageView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    crate::history::page(
        &st.db,
        &session,
        &branch,
        crate::history::PageOptions {
            before: input.before,
            limit: input.limit.and_then(|n| usize::try_from(n).ok()),
            kinds: input.kinds,
            query: None,
        },
    )
    .await
    .map_err(history_error)
}

/// `sessions.history.search` — ported from [`search_session_history`].
async fn op_history_search(
    context: OperationContext,
    input: ops::history::search::Input,
) -> ApiResult<HistoryPageView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    crate::history::page(
        &st.db,
        &session,
        &branch,
        crate::history::PageOptions {
            before: input.before,
            limit: input.limit.and_then(|n| usize::try_from(n).ok()),
            kinds: input.kinds,
            query: Some(input.q),
        },
    )
    .await
    .map_err(history_error)
}

/// A private, local twin of `branches.rs`'s `CALM_STATUS` — that one isn't
/// visible here, and status is small enough not to be worth sharing.
const CALM_STATUS: &str = "ok";

/// `sessions.status.get` — the session-scoped read `branches.rs`'s
/// `get_branch` already serves for a branch key; the attention level and
/// status message both live on `BranchView` (`tags` and `description`).
async fn op_status_get(
    context: OperationContext,
    input: ops::status::get::Input,
) -> ApiResult<BranchView> {
    let (_, branch) = require_session(&context.state.db, &input.session).await?;
    super::branch_view(&context.state.db, &branch).await
}

/// `sessions.status.set` — the same write `branches.status.set` performs,
/// resolved from a session id instead of a branch key.
async fn op_status_set(
    context: OperationContext,
    input: ops::status::set::Input,
) -> ApiResult<BranchView> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    let level = input.level.trim().to_ascii_lowercase();
    if level != CALM_STATUS && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        return Err(AppError::bad_request(format!(
            "unknown status '{level}' — expected one of {CALM_STATUS}, {}",
            tags::ATTENTION_VALUES.join(", ")
        )));
    }
    let message = input
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if let Some(message) = message {
        branch_mod::set_description(&st.db, &branch.id, message).await?;
    }
    let value = if level == CALM_STATUS {
        tags::clear(&st.db, &branch.id, tags::ATTENTION_KEY).await?;
        String::new()
    } else {
        tags::set(&st.db, &branch.id, tags::ATTENTION_KEY, &level, "", "agent").await?;
        level.clone()
    };
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "tag",
        json!({
            "key": tags::ATTENTION_KEY,
            "value": value,
            "note": message.unwrap_or_default(),
            "by": "agent",
        }),
    )
    .await?;
    if let Some(channel_id) =
        crate::channels::session_channel_for_branch(&st.db, &branch.id).await?
    {
        let urgency = crate::channels::Urgency::from_status_level(&level);
        let author =
            crate::channels::Subject::new(crate::channels::SubjectKind::Session, &channel_id);
        crate::channels::append(
            &st.db,
            &channel_id,
            crate::channels::NewMessage {
                kind: crate::channels::MessageKind::Status,
                urgency,
                author: &author,
                body: message.unwrap_or(&level),
                payload: &json!({ "level": level }),
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await?;
    }
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    super::branch_view(&st.db, &branch).await
}

/// `sessions.tags.list` — session tags live on the branch row, so the view is
/// `BranchView` (its `tags` field), not a bespoke tag list type.
async fn op_tags_list(
    context: OperationContext,
    input: ops::tags::list::Input,
) -> ApiResult<BranchView> {
    let (_, branch) = require_session(&context.state.db, &input.session).await?;
    super::branch_view(&context.state.db, &branch).await
}

/// `sessions.tags.set` — ported from [`set_session_tag`], returning
/// `BranchView` (the operation's declared `Output`) rather than the full
/// `SessionView` the old handler returned.
async fn op_tags_set(
    context: OperationContext,
    input: ops::tags::set::Input,
) -> ApiResult<BranchView> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    let tag_key = input.key.as_str();
    let value = input.value.trim();
    if crate::github::is_reserved_tag(tag_key) {
        return Err(AppError::bad_request(format!(
            "'{tag_key}' is loom-internal bookkeeping — it can be cleared, not set by hand"
        )));
    }
    if tag_key == tags::GITHUB_KEY && crate::github::parse_wiring(value).is_none() {
        return Err(AppError::bad_request(format!(
            "invalid value '{value}' for '{tag_key}' — expected owner/name#number"
        )));
    }
    if !tags::is_valid_value(tag_key, value) {
        return Err(AppError::bad_request(if tags::is_loud(tag_key) {
            format!(
                "invalid value '{value}' for '{tag_key}' — expected one of {} (clear the tag to return to calm)",
                tags::ATTENTION_VALUES.join(", ")
            )
        } else {
            format!("invalid value '{value}' for '{tag_key}' — must be non-empty")
        }));
    }
    let by = author_or_manual(input.by.as_deref());
    let note = input.note.trim();
    tags::set(&st.db, &branch.id, tag_key, value, note, &by).await?;
    events::record_tag(&st.db, &st.bus, &branch.id, tag_key, value, note, &by)
        .await
        .ok();
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    super::branch_view(&st.db, &branch).await
}

/// `sessions.tags.delete` — ported from [`clear_session_tag`], returning
/// `BranchView` rather than the old handler's `SessionView`.
async fn op_tags_delete(
    context: OperationContext,
    input: ops::tags::delete::Input,
) -> ApiResult<BranchView> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    let by = author_or_manual(input.by.as_deref());
    tags::clear(&st.db, &branch.id, &input.key).await?;
    events::record_tag(&st.db, &st.bus, &branch.id, &input.key, "", "", &by)
        .await
        .ok();
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    super::branch_view(&st.db, &branch).await
}

/// `sessions.launches.resolve` — ported from
/// [`crate::web::launches::resolve_session_launch`], resolved from typed input
/// rather than a raw JSON body.
async fn op_launches_resolve(
    context: OperationContext,
    input: ops::launches::resolve::Input,
) -> ApiResult<ResolvedLaunchView> {
    let st = &context.state;
    let profile_name = match input.selection.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(profile_name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    Ok(super::launches::resolve_launch(
        st,
        &input.selection,
        &crate::launch::ResolveOptions::default(),
    )
    .await?
    .view)
}

/// `sessions.adopt` — ported from [`adopt_session`].
async fn op_adopt(context: OperationContext, input: ops::adopt::Input) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    adopt(st, &session, &branch).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.archive` — ported from [`archive_session`] and
/// [`archive_launch_attempt`], folded into one typed result rather than the
/// old handler's ad hoc JSON object.
async fn op_archive(
    context: OperationContext,
    input: ops::archive::Input,
) -> ApiResult<SessionArchiveResult> {
    let st = &context.state;
    let (session, branch) = match require_session(&st.db, &input.session).await {
        Ok(found) => found,
        Err(error) if error.is_not_found() => {
            return op_archive_launch_attempt(st, &input.session).await;
        }
        Err(error) => return Err(error),
    };
    let warnings = archive(st, &session, &branch).await.map_err(|error| {
        AppError::internal(
            format!(
                "Could not finish archiving session {}. Its branch and conversation are safe; retry in a moment.",
                session.id
            ),
            error,
        )
    })?;
    Ok(SessionArchiveResult {
        archived: true,
        kind: "session".to_string(),
        branch: branch.branch,
        warnings,
    })
}

/// The `sessions.archive` counterpart of [`archive_launch_attempt`]: same
/// escape hatch for a reservation that never became a session, wired to this
/// operation's own result type.
async fn op_archive_launch_attempt(
    st: &AppState,
    session_id: &str,
) -> ApiResult<SessionArchiveResult> {
    if crate::runs::list_for_session(&st.db, session_id)
        .await?
        .is_empty()
    {
        return Err(AppError::not_found("session"));
    }
    crate::runs::cancel_for_session_with_summary(
        &st.db,
        session_id,
        "launch attempt archived by user",
    )
    .await?;
    let warnings = crate::session_manager::teardown_reserved_runtime(session_id).await;
    st.ide.kill(session_id);
    crate::auth::revoke_session_tokens(&st.db, session_id)
        .await
        .ok();
    Ok(SessionArchiveResult {
        archived: true,
        kind: "launch_attempt".to_string(),
        branch: session_id.to_string(),
        warnings,
    })
}

/// `sessions.recover` — ported from [`recover_session`].
async fn op_recover(
    context: OperationContext,
    input: ops::recover::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    if session.status == "archived" {
        recover(st, &session, &branch).await?;
    } else {
        crate::lifecycle::recover_acp_runtime(st, &session).await?;
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.handoff` — ported from [`handoff_session`].
async fn op_handoff(
    context: OperationContext,
    input: ops::handoff::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (initial_session, _) = require_session(&st.db, &input.session).await?;
    let req = HandoffReq {
        agent: input.agent,
        model: input.model,
        effort: input.effort,
        mode: input.mode,
        selection: input.selection,
        expected_profile_revision: input.expected_profile_revision,
        expected_resolver_revision: input.expected_resolver_revision,
    };
    let (session, branch) = crate::handoff::handoff_session(st, initial_session, req)
        .await
        .map_err(map_handoff_error)?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.chat` — ported from [`get_session_chat`].
async fn op_chat(context: OperationContext, input: ops::chat::Input) -> ApiResult<SessionChatView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let before = match (input.before_turn, input.before_seq) {
        (Some(turn), Some(seq)) => Some((turn, seq)),
        (None, None) => None,
        _ => {
            return Err(AppError::bad_request(
                "before_turn and before_seq must be supplied together",
            ))
        }
    };
    let (blocks, has_more) =
        crate::chat::list_page(&st.db, &session.id, before, CHAT_PAGE_SIZE).await?;
    let older_cursor = if has_more {
        blocks.first().map(|block| ChatCursorView {
            turn: block.turn,
            seq: block.seq,
        })
    } else {
        None
    };
    let metadata = match st.acp.get(&session.id) {
        Some(handle) => handle.metadata(),
        None => match session_mod::get_acp_metadata(&st.db, &session.id).await? {
            Some(raw) => serde_json::from_str(&raw)?,
            None => crate::acp::AcpMetadata::default(),
        },
    };
    let pending_prompt = session
        .pending_prompt
        .as_deref()
        .filter(|pending| !pending.trim().is_empty())
        .map(str::to_string);
    Ok(SessionChatView {
        blocks: blocks
            .into_iter()
            .map(|block| ChatBlockView {
                turn: block.turn,
                seq: block.seq,
                kind: block.kind,
                payload: block.payload,
                created_at: block.created_at,
            })
            .collect(),
        older_cursor,
        live_turn: session_mod::acp_inflight_turn(&session),
        effective_mode: effective_turn_mode(&session),
        pending_prompt,
        metadata: AcpMetadataView {
            commands: metadata.commands,
            config_options: metadata.config_options,
            modes: metadata.modes,
            steering_supported: metadata.steering_supported,
        },
    })
}

/// `sessions.conversation` — ported from [`conversation_session`]. Eliding
/// oversized tool payloads still runs on the blocking pool since it walks and
/// re-stringifies every block; the JSON encoding itself now happens in the
/// dispatcher rather than alongside it, since a registered operation returns a
/// typed value rather than a hand-built `Response`.
async fn op_conversation(
    context: OperationContext,
    input: ops::conversation::Input,
) -> ApiResult<weaver_core::transcript::iris::Log> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    let mut log = crate::chatlog::conversation(&st.db, &session, &branch)
        .await
        .ok_or_else(|| AppError::not_found("conversation"))?;
    let session_id = session.id.clone();
    tokio::task::spawn_blocking(move || {
        crate::chatlog::elide_tool_payloads(&mut log, &session_id);
        log
    })
    .await
    .map_err(|error| AppError::internal("conversation processing failed", error))
}

/// `sessions.files` — ported from [`list_session_files`].
async fn op_files(
    context: OperationContext,
    input: ops::files::Input,
) -> ApiResult<SessionFilesView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    let out = tokio::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(&session.work_dir)
        .output()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !out.status.success() {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            format!(
                "git ls-files failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    let needle = input.q.trim().to_ascii_lowercase();
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| String::from_utf8(raw.to_vec()).ok())
        .filter(|path| needle.is_empty() || path.to_ascii_lowercase().contains(&needle))
        .collect();
    files.sort_by_key(|path| {
        let lower = path.to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        (
            !lower.starts_with(&needle),
            !name.starts_with(&needle),
            path.len(),
            lower,
        )
    });
    files.truncate(40);
    Ok(SessionFilesView { files })
}

/// `sessions.mode` — ported from [`set_mode`].
async fn op_mode(
    context: OperationContext,
    input: ops::mode::Input,
) -> ApiResult<SessionModeResult> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    if session.policy_restricted && input.mode_id != "default" {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "restricted sessions cannot change permission mode",
        ));
    }
    let handle = require_acp_task(st, &session)?;
    let by = author_or_manual(input.by.as_deref());
    handle
        .set_mode(input.mode_id.clone(), Some(by.clone()))
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({ "by": by, "mode": input.mode_id }),
    )
    .await
    .ok();
    Ok(SessionModeResult {
        mode_id: input.mode_id,
    })
}

/// `sessions.raw` — ported from [`raw_session`]. JSON cannot carry raw bytes,
/// so the wire body carries base64 rather than the REST route's octet stream.
async fn op_raw(
    context: OperationContext,
    input: ops::raw::Input,
) -> ApiResult<SessionRawFileView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let rel = rel_path(&input.path)?;
    let bytes = match tokio::fs::read(work_dir.join(&rel)).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::not_found("file"))
        }
        Err(e) => return Err(e.into()),
    };
    Ok(SessionRawFileView {
        content_type: content_type_for(&rel).to_string(),
        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// `sessions.url` — ported from [`session_url_route`]. The registered
/// operation runs without the caller's `Host` header (the dispatcher hands
/// handlers typed input, not a request), so this can only resolve the
/// configured `auth.base_url` or the address the server is bound to — not a
/// browser's own Host the way the REST route it mirrors can.
async fn op_url(context: OperationContext, input: ops::url::Input) -> ApiResult<SessionUrlView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    let base = super::auth::public_base(st, &header::HeaderMap::new()).await;
    Ok(SessionUrlView {
        url: crate::links::session_url(&base, &session.id),
    })
}

/// `sessions.ide_info` — ported from [`crate::ide::info`]. `IdeInfo`'s fields
/// are private to `loom-editor`, so the response is round-tripped through JSON
/// into this bundle's own DTO rather than constructed field-by-field.
async fn op_ide_info(
    context: OperationContext,
    _input: ops::ide_info::Input,
) -> ApiResult<SessionIdeInfoView> {
    let editor = context.state.editor_state();
    let info = crate::ide::info(axum::extract::State(editor)).await.0;
    let value = serde_json::to_value(info)?;
    Ok(serde_json::from_value(value)?)
}

/// `sessions.shells.list` — ported from [`list_session_shells`].
async fn op_shells_list(
    context: OperationContext,
    input: ops::shells::list::Input,
) -> ApiResult<Vec<u32>> {
    let (session, _) = require_session(&context.state.db, &input.session).await?;
    Ok(crate::shell::list_debug(&session.id).await)
}

/// `sessions.shells.delete` — ported from [`delete_session_shell`]. Idempotent:
/// a missing shell is a no-op. Returns the indices still live, so the tab strip
/// refreshes without a second call — the legacy route returned `{closed: true}`,
/// which told a caller nothing it did not already know.
async fn op_shells_delete(
    context: OperationContext,
    input: ops::shells::delete::Input,
) -> ApiResult<Vec<u32>> {
    let (session, _) = require_session(&context.state.db, &input.session).await?;
    crate::shell::kill_debug(&session.id, input.index).await;
    Ok(crate::shell::list_debug(&session.id).await)
}

/// `sessions.update` — ported from [`patch_session`]. The legacy body's
/// `park`/`sort_order` compatibility fields (always rejected with a fixed
/// error) are not part of this operation's input at all: they existed only so
/// an old frontend payload failed loudly instead of being silently ignored,
/// and a caller of this operation never sends them.
async fn op_update(context: OperationContext, input: ops::update::Input) -> ApiResult<SessionView> {
    let st = &context.state;
    let (initial_session, _) = require_session(&st.db, &input.session).await?;
    let _source_permit = st.launch_gate.acquire_session(&initial_session.id).await;
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((session, branch)) = session_mod::with_branch(&st.db, &initial_session.id).await?
    else {
        return Err(AppError::conflict(
            "session changed while the update was waiting; review it again",
        ));
    };
    if let Some(title) = &input.title {
        let title = branch_mod::sanitize_user_title(title)
            .ok_or_else(|| AppError::bad_request("title must not be empty"))?;
        let expected_title = input.expected_title.as_deref().ok_or_else(|| {
            AppError::bad_request("expected_title is required when renaming a session")
        })?;
        let expected_provenance = input
            .expected_title_provenance
            .as_deref()
            .ok_or_else(|| {
                AppError::bad_request(
                    "expected_title_provenance is required when renaming a session",
                )
            })?
            .parse::<TitleProvenance>()
            .map_err(AppError::bad_request)?;
        match branch_mod::replace_title(
            &st.db,
            &branch.id,
            expected_title,
            expected_provenance,
            &title,
            TitleProvenance::User,
        )
        .await?
        {
            TitleUpdate::Applied(_) => {
                crate::channels::update_branch_channel_names(&st.db, &branch.id, &title).await?;
            }
            TitleUpdate::Stale(current) => {
                return Err(AppError::conflict(
                    "the task label changed while it was being edited; review it and retry",
                )
                .with_fields(json!({ "branch": super::branch_view(&st.db, &current).await? })));
            }
            TitleUpdate::Missing => return Err(AppError::not_found("branch")),
        }
    }
    if let Some(goal) = &input.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
        session_mod::bump_mutation_revision(&st.db, &session.id).await?;
        crate::channels::update_session_goal(&st.db, &session.id, goal).await?;
        tokio::fs::write(db::run_dir(&session.id).join("goal.txt"), goal)
            .await
            .ok();
    }
    if let Some(description) = &input.description {
        branch_mod::set_description(&st.db, &branch.id, description).await?;
    }
    if let Some(status) = &input.status {
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
    session_view(&st.db, &session, &branch).await
}

/// `sessions.delete` — ported from [`delete_session`].
async fn op_delete(
    context: OperationContext,
    input: ops::delete::Input,
) -> ApiResult<ops::delete::DeleteResult> {
    let st = &context.state;
    let (session, branch) = match require_session(&st.db, &input.session).await {
        Ok(found) => found,
        Err(error) if error.is_not_found() => {
            return op_delete_launch_attempt(st, &input.session).await;
        }
        Err(error) => return Err(error),
    };
    let warnings = remove(st, &session, &branch, input.keep_branch).await?;
    Ok(ops::delete::DeleteResult {
        deleted: true,
        kind: "session".to_string(),
        warnings,
    })
}

/// The `sessions.delete` counterpart of [`delete_launch_attempt`]: same
/// escape hatch [`op_archive_launch_attempt`] uses for `sessions.archive`,
/// wired to this operation's own result type.
async fn op_delete_launch_attempt(
    st: &AppState,
    session_id: &str,
) -> ApiResult<ops::delete::DeleteResult> {
    if crate::runs::list_for_session(&st.db, session_id)
        .await?
        .is_empty()
    {
        return Err(AppError::not_found("session"));
    }
    crate::runs::cancel_for_session_with_summary(
        &st.db,
        session_id,
        "launch attempt removed by user",
    )
    .await?;
    let warnings = crate::session_manager::teardown_reserved_runtime(session_id).await;
    st.ide.kill(session_id);
    crate::auth::revoke_session_tokens(&st.db, session_id)
        .await
        .ok();
    crate::runs::delete_for_session(&st.db, session_id).await?;
    Ok(ops::delete::DeleteResult {
        deleted: true,
        kind: "launch_attempt".to_string(),
        warnings,
    })
}

/// `sessions.config.set` — ported from [`set_config_option`].
async fn op_config_set(
    context: OperationContext,
    input: ops::config::set::Input,
) -> ApiResult<ops::config::set::ConfigOptionResult> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let handle = require_acp_task(st, &session)?;
    let metadata = handle
        .set_config_option(input.config_id.clone(), input.value.clone())
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(ops::config::set::ConfigOptionResult {
        config_id: input.config_id,
        value: input.value,
        metadata: AcpMetadataView {
            commands: metadata.commands,
            config_options: metadata.config_options,
            modes: metadata.modes,
            steering_supported: metadata.steering_supported,
        },
    })
}

/// `sessions.conversation.block` — ported from [`conversation_block`].
async fn op_conversation_block(
    context: OperationContext,
    input: ops::conversation::block::Input,
) -> ApiResult<weaver_core::transcript::iris::Block> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    let log = crate::chatlog::conversation(&st.db, &session, &branch)
        .await
        .ok_or_else(|| AppError::not_found("conversation"))?;
    log.messages
        .get(input.message as usize)
        .and_then(|m| m.blocks.get(input.block as usize))
        .cloned()
        .ok_or_else(|| AppError::not_found("conversation block"))
}

/// `sessions.github.refresh` — ported from [`refresh_github_session`].
async fn op_github_refresh(
    context: OperationContext,
    input: ops::github::refresh::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    github::refresh(st, &session, &branch, false)
        .await
        .map_err(|e| github_request_error("refresh this pull request", e))?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.github.set` — ported from [`set_github_session`].
async fn op_github_set(
    context: OperationContext,
    input: ops::github::set::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    if input.pr_number <= 0 {
        return Err(AppError::bad_request("PR number must be positive"));
    }
    let repo_root = PathBuf::from(&branch.repo_root);
    let snap = github::fetch_pr(st, &repo_root, input.pr_number)
        .await
        .map_err(|e| github_request_error("find this pull request", e))?
        .ok_or_else(|| {
            AppError::bad_request(format!("pull request #{} was not found", input.pr_number))
        })?;
    github::set_mapping(&st.db, &branch.id, input.pr_number).await?;
    github::apply_snapshot(st, &session, &branch, &snap, false).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.github.clear` — ported from [`clear_github_session`].
async fn op_github_clear(
    context: OperationContext,
    input: ops::github::clear::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    github::clear_mapping(&st.db, &branch.id).await?;
    github::clear_status(&st.db, &branch.id).await?;
    if let Err(e) = github::refresh(st, &session, &branch, false).await {
        tracing::debug!(branch = %branch.branch, error = %e, "automatic PR refresh after clearing mapping failed");
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.github.labels.add` — ported from [`add_github_session_labels`].
async fn op_github_labels_add(
    context: OperationContext,
    input: ops::github::labels::add::Input,
) -> ApiResult<ops::github::labels::add::AddLabelsResult> {
    let st = &context.state;
    let (_, branch) = require_session(&st.db, &input.session).await?;
    if input.labels.is_empty() || input.labels.len() > 10 {
        return Err(AppError::bad_request(
            "GitHub labels must contain between 1 and 10 entries",
        ));
    }
    let labels = input
        .labels
        .into_iter()
        .map(|label| label.trim().to_string())
        .collect::<Vec<_>>();
    if labels
        .iter()
        .any(|label| label.is_empty() || label.len() > 100)
    {
        return Err(AppError::bad_request(
            "each GitHub label must contain between 1 and 100 bytes",
        ));
    }
    let status = github::get_status(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::bad_request("session has no associated pull request"))?;
    let repo_root = PathBuf::from(&branch.repo_root);
    let slug = crate::repo::github_slug_for_root(&st.db, &repo_root)
        .await?
        .ok_or_else(|| AppError::bad_request("session repository has no GitHub identity"))?;
    let repo = crate::repo::parse_slug(&slug)
        .map_err(|_| AppError::bad_request("session GitHub repository is invalid"))?;
    let app = super::configured_github_app(st).await?;
    app.add_thread_labels(&repo, status.pr_number, &labels)
        .await
        .map_err(|e| github_request_error("label this pull request", e))?;
    Ok(ops::github::labels::add::AddLabelsResult {
        number: status.pr_number,
        labels,
    })
}

/// `sessions.handoff.resolve` — ported from [`resolve_session_handoff`].
async fn op_handoff_resolve(
    context: OperationContext,
    input: ops::handoff::resolve::Input,
) -> ApiResult<ResolvedLaunchView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    crate::handoff::resolve_session_handoff(st, &session, &input.selection)
        .await
        .map_err(map_handoff_error)
}

/// `sessions.prompt.create` — ported from [`prompt_session`]. `by` is not
/// read from the caller — see the operation's own doc comment. Provenance is
/// derived from the credential the same way `set_issue_tag_operation` in
/// `web/issues.rs` derives its `by`: `manual` for a human operator, `agent`
/// otherwise.
async fn op_prompt_create(
    context: OperationContext,
    input: ops::prompt::create::Input,
) -> ApiResult<ops::prompt::create::PromptResult> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let handle = require_acp_task(st, &session)?;
    let by = if context.principal.is_human() {
        "manual"
    } else {
        "agent"
    }
    .to_string();
    let audit_text = if input.force_queued {
        session_mod::read_pending_prompt(&st.db, &session.id).await?
    } else {
        input.text.clone()
    };
    let ack = if input.force_queued {
        handle.force_pending(Some(by.clone())).await
    } else {
        let resources = prompt_resources(&session.work_dir, &input.files).await?;
        if input.send_now {
            handle
                .stop_and_send(input.text.clone(), Some(by.clone()), resources)
                .await
        } else {
            handle
                .prompt(input.text.clone(), Some(by.clone()), resources)
                .await
        }
    }
    .map_err(|e| AppError::conflict(e.to_string()))?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({
            "by": by,
            "text": audit_text,
            "send_now": input.send_now,
            "promoted_queue": input.force_queued,
        }),
    )
    .await
    .ok();
    Ok(ops::prompt::create::PromptResult {
        queued: ack.queued,
        turn: ack.turn,
    })
}

/// `sessions.prompt.retract` — ported from [`retract_queued_prompt`].
async fn op_prompt_retract(
    context: OperationContext,
    input: ops::prompt::retract::Input,
) -> ApiResult<ops::prompt::retract::RetractResult> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let handle = require_acp_task(st, &session)?;
    let text = handle
        .retract_pending()
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(ops::prompt::retract::RetractResult { text })
}

/// `sessions.resumption_cue.get` — ported from [`get_resumption_cue`].
async fn op_resumption_cue_get(
    context: OperationContext,
    input: ops::resumption_cue::get::Input,
) -> ApiResult<ResumptionCueView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    Ok(crate::metadata_assist::current_cue(&st.db, &session, &branch).await?)
}

/// `sessions.resumption_cue.ensure` — ported from [`ensure_resumption_cue`].
async fn op_resumption_cue_ensure(
    context: OperationContext,
    input: ops::resumption_cue::ensure::Input,
) -> ApiResult<ResumptionCueView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    Ok(crate::metadata_assist::ensure_cue(&st.db, &st.acp, &session, &branch, input.force).await?)
}

/// `sessions.permissions.answer` — ported from [`answer_permission`]. The
/// legacy handler's `principal.is_human()` refusal is now `actor = User` on
/// the declaration; the inline check is deleted rather than ported, matching
/// `auth::automation_token_op`'s treatment of its own former `is_admin` check.
async fn op_permissions_answer(
    context: OperationContext,
    input: ops::permissions::answer::Input,
) -> ApiResult<ops::permissions::answer::AnswerPermissionResult> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    require_acp(&session)?;
    let handle = require_acp_task(st, &session)?;
    let by = author_or_manual(input.by.as_deref());
    match handle
        .answer_permission(
            input.request_id.clone(),
            input.option_id.clone(),
            by.clone(),
        )
        .await
        .map_err(|e| AppError::conflict(e.to_string()))?
    {
        crate::acp::PermAnswer::Ok => {
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "permission",
                json!({ "by": by, "request_id": input.request_id, "option_id": input.option_id }),
            )
            .await
            .ok();
            Ok(ops::permissions::answer::AnswerPermissionResult {
                resolved: true,
                option_id: input.option_id,
            })
        }
        crate::acp::PermAnswer::NotFound => Err(AppError::not_found("permission request")),
        crate::acp::PermAnswer::AlreadyResolved => {
            Err(AppError::conflict("permission request already resolved"))
        }
    }
}

/// `sessions.title.regenerate` — ported from [`regenerate_session_title`].
async fn op_title_regenerate(
    context: OperationContext,
    input: ops::title::regenerate::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    crate::metadata_assist::spawn_title_generation(
        st.db.clone(),
        st.bus.clone(),
        st.acp.clone(),
        session.clone(),
        branch,
        true,
    )
    .await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}

/// `sessions.title.generation.set` — ported from [`set_session_title_generation`].
async fn op_title_generation_set(
    context: OperationContext,
    input: ops::title::generation::set::Input,
) -> ApiResult<SessionView> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    crate::metadata_assist::set_title_enabled(&st.db, &session.id, input.enabled).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    session_view(&st.db, &session, &branch).await
}
