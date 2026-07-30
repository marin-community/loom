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
use crate::session::{self as session_mod, NewSession, Session};
use crate::{agent, backend, config, custom_agents, db, events, git, github, repo};
use weaver_api::{
    CreateReq, EnsureResumptionCueReq, HandoffReq, HistoryPageView, LaunchOverrides,
    LaunchSelection, PatchSessionReq, ResumptionCueView, SearchSessionsOptions, SendReq,
    SessionSearchAttention, SessionSearchStatus, SessionSummaryView, SessionView, SetTagsReq,
    SetTitleGenerationReq, TagReq,
};
use weaver_core::branch as branch_mod;
use weaver_core::branch::{Branch, TitleProvenance, TitleUpdate};
use weaver_core::tags;
use weaver_core::watch::{self as watch_store, Watch};

use super::{
    author_or_manual, require_branch, require_session, session_summary_view, session_view,
};
use super::{ApiResult, AppError, AppState};
use crate::runtime::{layer_launch_environment, repo_cfg_or_default, set_env};

async fn delete_session_row(st: &AppState, session_id: &str) -> Result<(), AppError> {
    if let Some(revision) = session_mod::delete(&st.db, session_id).await? {
        crate::session_layout::publish_invalidation(&st.db, &st.bus, revision).await;
    }
    Ok(())
}
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
    /// Compatibility filter for automation-class sessions. Omission retains
    /// the historical interactive-only inventory; fleet workbenches opt in.
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
}

pub(super) async fn list_sessions(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<ListSessionsQuery>,
) -> ApiResult<Json<Vec<SessionView>>> {
    if q.managed && !principal.is_admin() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "admin grant required to list managed sessions",
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
}

/// Compact polling/search contract for indexes. Full session context remains on
/// `GET /api/sessions/{id}` and is fetched only when a row or page discloses it.
pub(super) async fn list_session_summaries(
    State(st): State<AppState>,
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
    (managed || !warm.contains(&session.id))
        && (filter.archived || session.status != "archived")
        && (!filter.archived_only || session.status == "archived")
        && (filter.automation || session.class != "automation")
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
        json!({ "url": super::session_url(&base, &session.id) }),
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
    let actor = crate::provision::Actor::from_principal(&principal, delegated);
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
        crate::auth::Grant::Admin => {}
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

/// Build the explicit ambient baseline used when Tapestry clears inheritance.
/// Profile/repo values win over baseline and allowlisted ambient values; loom's
/// own session variables are injected later by `agent::session_env`.
async fn resume_environment(
    db: &Db,
    session: &Session,
    repo_root: &std::path::Path,
    cfg: &weaver_core::repo_config::RepoConfig,
) -> Vec<(String, String)> {
    let env = crate::runtime::launch_environment(
        db,
        repo_root,
        cfg,
        &session.profile,
        session.policy_strict,
        session.policy_restricted,
    )
    .await;
    if !session.policy_env_clear {
        return env;
    }
    let allowlist =
        serde_json::from_str::<Vec<String>>(&session.policy_ambient_allowlist).unwrap_or_default();
    crate::profile::cleared_environment(env, &allowlist)
}

async fn rotate_session_token(
    db: &Db,
    session: &Session,
    env: &mut Vec<(String, String)>,
) -> ApiResult<()> {
    crate::auth::revoke_session_tokens(db, &session.id).await?;
    let token = crate::auth::create_session_token(
        db,
        session.created_by.as_deref(),
        &session.id,
        &session.branch_id,
    )
    .await?;
    set_env(env, "LOOM_TOKEN", token);
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
        crate::metadata_assist::ensure_cue(&st.db, &session, &branch, req.force).await?,
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
pub(crate) async fn archive(
    st: &AppState,
    session: &Session,
    _branch: &Branch,
) -> Result<Vec<String>, AppError> {
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    archive_locked(st, &current_session, &current_branch).await
}

/// Archive from a retention/integration path unless this branch carries the
/// explicit `auto-archive: disabled` opt-out. The check and teardown share the
/// lifecycle lock, so setting the label before an automatic operation acquires
/// the lock reliably prevents that operation; manual [`archive`] ignores it.
pub(crate) async fn auto_archive(
    st: &AppState,
    session: &Session,
    _branch: &Branch,
) -> Result<Option<Vec<String>>, AppError> {
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    if tags::auto_archive_disabled(&st.db, &current_branch.id).await? {
        tracing::info!(
            session = %current_session.id,
            branch = %current_branch.id,
            "automatic archive skipped by auto-archive: disabled tag"
        );
        return Ok(None);
    }
    archive_locked(st, &current_session, &current_branch)
        .await
        .map(Some)
}

/// Shared teardown after the caller has acquired the runtime lifecycle lock and
/// refreshed the session row.
async fn archive_locked(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<Vec<String>, AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, "archiving session");
    let mut warnings: Vec<String> = Vec::new();

    // Capture the agent's conversation log before teardown. The transcript lives
    // outside the worktree so it would survive removal, but capturing first keeps
    // it whole regardless. Best-effort: failures are warnings, never fatal.
    let (_, log_warnings) = crate::chatlog::capture(&st.db, session, branch).await;
    warnings.extend(log_warnings);
    tracing::debug!(session = %session.id, "captured conversation transcript before teardown");

    // Cancellation is the durable boundary: an automation request that
    // finishes provisioning after this point cannot promote itself.
    crate::runs::cancel_for_session_with_summary(&st.db, &session.id, "session archived").await?;
    // The row must never say `archived` while its supervisor is still live.
    // A tapestry kill is acknowledged before the socket disappears, so wait
    // for teardown and fail without flipping the row if it cannot complete.
    backend::kill_session_and_wait(&session.term_session).await?;
    // The killed relay makes its ACP task exit; remove any handle that has not
    // observed that edge yet. For a terminal session this is a no-op.
    if session.protocol == "acp" {
        st.acp.stop(&session.id);
    }
    crate::auth::revoke_session_tokens(&st.db, &session.id).await?;
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    tracing::debug!(session = %session.id, "killed terminal, debug shells, and ide sessions");
    if work_dir.exists() {
        tracing::debug!(session = %session.id, work_dir = %work_dir.display(), "removing worktree");
        if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
            warnings.push(format!("worktree remove: {e}"));
            tokio::fs::remove_dir_all(&work_dir).await.ok();
        }
    }
    session_mod::set_status(&st.db, &session.id, "archived").await?;
    crate::channels::archive_session_channel(&st.db, &session.id).await?;
    // A torn-down session cannot keep owning work. Return every issue it held
    // to the repo backlog while preserving source-branch provenance and issue
    // status, just as full session deletion does.
    weaver_core::issue::unclaim_branch(&st.db, &branch.repo_root, &branch.branch).await?;
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
    if warnings.is_empty() {
        tracing::info!(session = %session.id, branch = %branch.id, "session archived");
    } else {
        tracing::warn!(branch = %branch.id, warnings = warnings.len(), "session archived with warnings");
    }
    Ok(warnings)
}

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
    let warnings = archive(&st, &session, &branch).await?;
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
/// will pick a freshly-merged PR up within a tick.
pub(super) async fn refresh_github_session(
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

#[derive(Debug, Deserialize)]
pub(super) struct GithubMappingBody {
    pub pr_number: i64,
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
    if !github::gh_available().await {
        return Err(AppError::bad_request(
            "the GitHub CLI (`gh`) is not available on the server",
        ));
    }
    let token = crate::agent_env::get(&st.db, "GH_TOKEN").await;
    let snap = github::fetch_pr(
        &PathBuf::from(&branch.repo_root),
        &req.pr_number.to_string(),
        token.as_deref(),
    )
    .await
    .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("gh: {e}")))?
    .ok_or_else(|| {
        AppError::bad_request(format!("pull request #{} was not found", req.pr_number))
    })?;
    github::set_mapping(&st.db, &branch.id, req.pr_number).await?;
    github::apply_snapshot(&st, &session, &branch, &snap, false).await?;
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
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
    if github::gh_available().await {
        if let Err(e) = github::refresh(&st, &session, &branch, false).await {
            tracing::debug!(branch = %branch.branch, error = %e, "automatic PR refresh after clearing mapping failed");
        }
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
}

/// Bring up an engine-managed (warm) session for a watch, reusing the same
/// branch/worktree/terminal launch machinery as an ordinary session — the only
/// differences are that it forks a dedicated `weaver/watch-<name>` branch
/// and the row is stamped `managed_by = watch.id` so the fleet listing and
/// every survey hide it.
///
/// A warm session is the watcher's own long-lived agent; its persistence across
/// rounds (the same terminal/worktree, resumed on adopt) is what gives the watch
/// across-round memory. The engine calls this once, on first need
/// ([`crate::watch::ensure_warm_session`]); thereafter it reuses the stored
/// session id.
pub(crate) async fn create_warm_session(
    st: &AppState,
    watch: &Watch,
    repo_root: &std::path::Path,
) -> Result<Session, AppError> {
    tracing::info!(watch = %watch.id, repo = %repo_root.display(), "creating warm session for watch");
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let selected_profile = match watch.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(selected_profile).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let selection = LaunchSelection {
        profile: selected_profile.to_string(),
        overrides: LaunchOverrides {
            model: (!watch.model.trim().is_empty()).then(|| watch.model.trim().to_string()),
            effort: (!watch.effort.trim().is_empty()).then(|| watch.effort.trim().to_string()),
            ..Default::default()
        },
    };
    let resolved = super::launches::resolve_launch(
        st,
        &selection,
        &crate::launch::ResolveOptions {
            default_class: Some("automation".to_string()),
            ..Default::default()
        },
    )
    .await?;
    if !resolved.view.valid {
        return Err(AppError::conflict(
            resolved
                .view
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "warm session launch is not currently admissible".to_string()),
        )
        .with_fields(json!({ "preview": resolved.view })));
    }
    let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let current_profile = crate::profile::get(&st.db, &resolved.profile.name)
        .await?
        .ok_or_else(|| AppError::conflict("watch profile changed during warm launch"))?;
    if current_profile.revision != resolved.view.profile_revision
        || current_profile.lifetime != resolved.view.profile_lifetime
    {
        return Err(AppError::conflict(
            "watch profile changed during warm launch; retry against a fresh resolution",
        ));
    }
    let launch_snapshot =
        crate::launch::serialize_snapshot(&resolved.view, resolved.custom_agent.as_ref())
            .map_err(|error| AppError::bad_request(error.to_string()))?;
    let custom_agent = resolved.custom_agent.clone();
    let launch_profile = resolved.profile;
    let agent = resolved.view.agent;
    let model = resolved.view.model;
    let effort = resolved.view.effort;
    let protocol = resolved.view.protocol;
    let mode = resolved.view.mode;
    let class = resolved.view.class;
    let stamped_allowed_tools = serde_json::to_string(&resolved.runtime_permissions)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let stamped_mcp_access = serde_json::to_string(&resolved.mcp_policy)
        .map_err(|error| AppError::bad_request(error.to_string()))?;

    let launch_permit = st.launch_gate.acquire(&repo_root).await;
    let repo_root_str = repo_root.display().to_string();
    let base = git::default_base(&repo_root).await?;

    // A stable, collision-resistant branch slug per watch; if an old warm
    // branch lingers (a prior warm session was archived), suffix to a fresh one.
    let base_slug = format!("watch-{}", branch_mod::slugify(&watch.name));
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
    tracing::info!(watch = %watch.id, branch = %branch_name, work_dir = %work_dir.display(), "provisioning worktree for warm session");
    git::worktree_add(&repo_root, &work_dir, &branch_name, &base)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    branch_mod::set_title(
        &st.db,
        &branch.id,
        &format!("watch {}", watch.name),
        TitleProvenance::Derived,
    )
    .await?;
    tracing::debug!(watch = %watch.id, branch = %branch.id, "upserted warm session branch row");

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    tracing::debug!(watch = %watch.id, session = %session_id, "allocated warm session id and run dir");

    let goal_file = match watch
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
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = layer_launch_environment(
        &st.db,
        &repo_root,
        &repo_cfg,
        &launch_profile.name,
        profile_environment,
        launch_profile.strict,
        launch_profile.restricted,
    )
    .await;
    if launch_profile.env_clear {
        let allowlist = launch_profile
            .ambient_names()
            .map_err(|error| AppError::bad_request(error.to_string()))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }

    // Persist before exposing the scoped credential to the child. Token lookup
    // deliberately requires a live bound session, so an eager agent cannot hit
    // a transient authentication failure during startup.
    let status = agent::initial_status(&st.db, &agent).await;
    let session = crate::session_layout::insert_session(
        &st.db,
        &st.bus,
        &NewSession {
            id: session_id.clone(),
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            term_session: term_session.clone(),
            agent_kind: agent.clone(),
            model: model.clone(),
            effort: effort.clone(),
            status: status.to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(watch.id.clone()),
            created_by: None,
            protocol: protocol.clone(),
            origin: "watch".to_string(),
            class: class.clone(),
            tracking_issue_id: None,
        },
        &session_mod::SessionLaunchPolicy {
            profile: launch_profile.name.clone(),
            launch_mode: mode.clone(),
            profile_revision: launch_profile.revision,
            profile_lifetime: launch_profile.lifetime,
            strict: launch_profile.strict,
            env_clear: launch_profile.env_clear,
            ambient_allowlist: launch_profile.ambient_allowlist.clone(),
            idle_archive_secs: resolved.view.policy.idle_archive_secs,
            turn_budget: resolved.view.policy.turn_budget.unwrap_or(0),
            prelude: launch_profile.prelude.clone(),
            restricted: launch_profile.restricted,
            allowed_tools: stamped_allowed_tools.clone(),
            mcp_access: stamped_mcp_access,
            launch_snapshot,
            creator_kind: "system".to_string(),
            creator_subject: format!("watch:{}", watch.id),
            parent_session_id: None,
            automation_run_id: None,
        },
    )
    .await?;
    let session_token =
        crate::auth::create_session_token(&st.db, None, &session_id, &branch.id).await?;
    set_env(&mut extra_env, "LOOM_TOKEN", session_token);
    tracing::info!(watch = %watch.id, session = %session_id, agent = %agent, protocol = %protocol, work_dir = %work_dir.display(), "launching warm session agent");
    let launch_result = if protocol == "acp" {
        match agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                session_id: &session.id,
                branch_id: &branch.id,
                runtime: &agent,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &model,
                effort: &effort,
                goal_file: goal_file.as_deref(),
                primer_file: None,
                extra_env: &extra_env,
                env_clear: launch_profile.env_clear,
                mode: &mode,
                prelude: &launch_profile.prelude,
                restricted: launch_profile.restricted,
                allowed_tools: &stamped_allowed_tools,
                mcp_access: &session.policy_mcp_access,
                custom: custom_agent.as_ref(),
            },
            agent::AcpOpen::Fresh,
        )
        .await
        {
            Ok(launch) => crate::acp::start(st, &session.id, launch).await,
            Err(error) => Err(error),
        }
    } else {
        agent::launch(
            &st.db,
            &agent::LaunchSpec {
                branch_id: &branch.id,
                runtime: &agent,
                work_dir: &work_dir,
                term_session: &term_session,
                goal_file: goal_file.as_deref(),
                primer_file: None,
                prelude: &launch_profile.prelude,
                server_addr: &st.addr,
                model: &model,
                effort: &effort,
                extra_env: &extra_env,
                env_clear: launch_profile.env_clear,
                custom: custom_agent.as_ref(),
            },
            agent::LaunchMode::Fresh,
        )
        .await
    };
    if let Err(error) = launch_result {
        crate::auth::revoke_session_tokens(&st.db, &session_id)
            .await
            .ok();
        st.acp.stop(&session_id);
        backend::kill_session(&term_session).await.ok();
        delete_session_row(st, &session_id).await.ok();
        return Err(AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        ));
    }
    tracing::info!(watch = %watch.id, session = %session_id, "warm session agent launched");
    drop(launch_permit);

    repo::record_use(&st.db, &repo_root_str).await.ok();
    tracing::info!(
        watch = %watch.id,
        session = %session.id,
        "warm session created"
    );
    Ok(session)
}

/// Guard for [`adopt`] and [`recover`]: 409 when a *different* session on the
/// same branch is still active. Archived no longer occupies the branch slot, so
/// the slot may have been re-let since this session left the fleet — resuming it
/// then would collide on the worktree path and the one-active-session-per-branch
/// index.
async fn require_branch_slot_free(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<(), AppError> {
    if let Some(other) = session_mod::active_for_branch(&st.db, &branch.id).await? {
        if other.id != session.id {
            return Err(AppError::conflict(format!(
                "branch '{}' already has an active session ({})",
                branch.branch, other.id
            )));
        }
    }
    Ok(())
}

/// Prove that a respawn still targets the profile lifetime accepted by this
/// session. A same-lifetime edit, credential rotation, or retirement remains
/// valid; a recreate under the same name does not.
async fn require_session_profile_lifetime(
    db: &Db,
    session: &Session,
) -> ApiResult<crate::profile::Profile> {
    let profile = crate::profile::get_including_retired(db, &session.profile)
        .await?
        .ok_or_else(|| {
            AppError::conflict(format!(
                "session '{}' profile lifetime is no longer available",
                session.profile
            ))
        })?;
    if session.profile_lifetime == 0 || profile.lifetime != session.profile_lifetime {
        return Err(AppError::conflict(format!(
            "session '{}' belongs to an unavailable profile lifetime; create a canonical replacement instead of reusing same-name credentials",
            session.id
        )));
    }
    Ok(profile)
}

fn stamped_custom_agent(session: &Session) -> ApiResult<Option<custom_agents::CustomAgent>> {
    if agent::builtin_agent_type(&session.agent_kind).is_some() {
        return Ok(None);
    }
    if session.launch_snapshot.trim().is_empty() {
        return Err(AppError::conflict(format!(
            "session '{}' has no captured custom-agent definition; create a canonical replacement instead of consulting the mutable registry",
            session.id
        )));
    }
    let snapshot =
        crate::launch::deserialize_snapshot(&session.launch_snapshot).map_err(|error| {
            AppError::conflict(format!(
                "session '{}' has an unreadable launch snapshot: {error}",
                session.id
            ))
        })?;
    let custom = snapshot.custom_agent.ok_or_else(|| {
        AppError::conflict(format!(
            "session '{}' has no captured custom-agent definition; create a canonical replacement instead of consulting the mutable registry",
            session.id
        ))
    })?;
    if custom.name != session.agent_kind {
        return Err(AppError::conflict(format!(
            "session '{}' captured custom agent '{}' but is stamped as '{}'",
            session.id, custom.name, session.agent_kind
        )));
    }
    Ok(Some(custom))
}

async fn require_resume_capacity(
    db: &Db,
    session: &Session,
    profile: &crate::profile::Profile,
) -> ApiResult<()> {
    if profile.max_concurrent <= 0 {
        return Ok(());
    }
    let active = crate::profile::active_count(db, &profile.name).await?;
    let keeps_existing_slot = crate::profile::status_consumes_capacity(&session.status);
    if !keeps_existing_slot && active >= profile.max_concurrent {
        return Err(AppError::conflict(format!(
            "profile '{}' has reached its max_concurrent limit ({})",
            profile.name, profile.max_concurrent
        )));
    }
    Ok(())
}

/// Recreate an orphaned session's terminal and resume its agent. The worktree is
/// expected to still be on disk (an orphaned session only lost its terminal); a
/// missing worktree is an error here — recovering a *torn-down* (archived)
/// session, which rebuilds the worktree first, goes through [`recover`].
pub(crate) async fn adopt(
    st: &AppState,
    session: &Session,
    _branch: &Branch,
) -> Result<(), AppError> {
    // Lock order shared with handoff/archive/delete: source session, global
    // lifecycle mutation, then profile lifetime/admission. Profile CRUD never
    // waits on a session/lifecycle lock, so this order cannot form a cycle.
    let _source_permit = st.launch_gate.acquire_session(&session.id).await;
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, _current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    let session = &current_session;
    let _profile_permit = st.launch_gate.acquire_profile(&session.profile).await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(AppError::not_found("session"));
    };
    let session = &current_session;
    let branch = &current_branch;
    let profile = require_session_profile_lifetime(&st.db, session).await?;
    require_resume_capacity(&st.db, session, &profile).await?;
    let custom_agent = stamped_custom_agent(session)?;
    require_branch_slot_free(st, session, branch).await?;
    if session.protocol == "acp" {
        return adopt_acp(
            st,
            session,
            branch,
            "session adopted",
            custom_agent.as_ref(),
        )
        .await;
    }
    tracing::info!(session = %session.id, branch = %branch.id, "adopting orphaned session");
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
    tracing::debug!(session = %session.id, work_dir = %work_dir.display(), "adopt preflight checks passed");
    // The post-flip conversion: a terminal session whose builtin runtime now
    // declares acp is adopted *into* acp rather than back onto a PTY. Claude
    // reopens its own on-disk conversation (the adapter's session ids are
    // claude's ids); codex — which never had a scoped terminal resume — starts
    // fresh from the goal file. Custom agents and any runtime still declaring
    // terminal keep the PTY relaunch.
    let runtime = session.agent_kind.clone();
    let declares_acp = session.launch_snapshot.trim().is_empty()
        && matches!(
            agent::metadata_for(&st.db, &runtime).await?,
            Some(meta) if meta.builtin && meta.protocol == "acp"
        );
    if declares_acp {
        return adopt_terminal_into_acp(st, session, branch, &runtime).await;
    }
    resume_agent(
        st,
        session,
        branch,
        "session adopted",
        custom_agent.as_ref(),
    )
    .await
}

/// Convert an orphaned terminal session to ACP on adopt: respawn as a relay +
/// adapter, reopening claude's own on-disk conversation via `session/load` when
/// one is recorded for the worktree (else a fresh session re-oriented from the
/// goal file). The chat journal starts empty either way — a load replay is
/// suppressed, and the terminal era lives in the captured transcript — but the
/// agent-side context survives in full. The acp task's handshake stamps the row
/// (`protocol='acp'` + the adapter session id) once the reopen acks.
async fn adopt_terminal_into_acp(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    runtime: &str,
) -> Result<(), AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, runtime = %runtime,
        "adopting terminal session into acp");
    let work_dir = PathBuf::from(&session.work_dir);
    let repo_root = PathBuf::from(&branch.repo_root);
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
    rotate_session_token(&st.db, session, &mut extra_env).await?;
    let run_dir = db::run_dir(&session.id);
    let primer_file = stamped_primer_file(&run_dir, &session.policy_prelude);
    let goal_file = {
        let f = run_dir.join("goal.txt");
        f.exists().then_some(f)
    };
    // A fresh relay: no spool cursor, no in-flight turn.
    session_mod::set_ack_seq(&st.db, &session.id, 0).await.ok();
    session_mod::set_inflight(&st.db, &session.id, None)
        .await
        .ok();
    let open = if runtime == "claude" {
        match agent::claude_projects_dir()
            .and_then(|d| agent::latest_claude_session_id(&d, &work_dir))
        {
            Some(id) => {
                tracing::info!(session = %session.id, claude_session = %id,
                    "reopening claude's on-disk conversation");
                agent::AcpOpen::Load(id)
            }
            None => agent::AcpOpen::Fresh,
        }
    } else {
        agent::AcpOpen::Fresh
    };
    let launch = agent::build_acp_launch(
        &st.db,
        &agent::AcpLaunchSpec {
            session_id: &session.id,
            branch_id: &branch.id,
            runtime,
            work_dir: &work_dir,
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            extra_env: &extra_env,
            env_clear: session.policy_env_clear,
            // Terminal rows carry no mode; on adoption they take the acp default.
            mode: agent::DEFAULT_ACP_MODE,
            prelude: &session.policy_prelude,
            restricted: session.policy_restricted,
            allowed_tools: &session.policy_allowed_tools,
            mcp_access: &session.policy_mcp_access,
            custom: None,
        },
        open,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::acp::start(st, &session.id, launch)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    session_mod::set_status(&st.db, &session.id, "running").await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": "running", "reason": "session adopted into acp" }),
    )
    .await
    .ok();
    Ok(())
}

/// Adopt an ACP session: respawn its relay + adapter and reopen the conversation.
/// When the relay supervisor is still alive but loom has no task for it (a crashed
/// task), just re-attach ([`crate::acp::attach`]). When the relay is gone, respawn
/// it and reopen via `session/load` (the adapter advertised `loadSession` and we
/// have its id), falling back to a fresh session re-oriented from the goal file.
async fn adopt_acp(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    reason: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> Result<(), AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, "adopting acp session");
    if st.acp.is_live(&session.id) {
        return Err(AppError::conflict("session already has a live ACP task"));
    }
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(AppError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            session.work_dir
        )));
    }

    if backend::has_session(&session.term_session).await {
        // The relay outlived a crashed task — re-attach from the persisted cursor.
        tracing::info!(session = %session.id, "acp relay alive; re-attaching");
        crate::acp::attach(st, &session.id)
            .await
            .map_err(|e| AppError::conflict(e.to_string()))?;
    } else {
        // The relay is gone — respawn the adapter and reopen the conversation.
        let repo_root = PathBuf::from(&branch.repo_root);
        let repo_cfg = repo_cfg_or_default(&repo_root);
        let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
        rotate_session_token(&st.db, session, &mut extra_env).await?;
        let runtime = session.agent_kind.clone();
        let (primer_file, goal_file) = resume_prompt_files(st, session, branch).await;
        let mode = session
            .current_mode
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| agent::DEFAULT_ACP_MODE.to_string());
        // A respawned relay has a fresh spool (seq 1..) and no in-flight turn —
        // reset the persisted cursor + inflight so a later attach replays cleanly.
        session_mod::set_ack_seq(&st.db, &session.id, 0).await.ok();
        session_mod::set_inflight(&st.db, &session.id, None)
            .await
            .ok();
        // Reopen via session/load where the adapter advertised it and we have an
        // id; otherwise a fresh session re-oriented from the goal file.
        let open = match session.acp_session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => agent::AcpOpen::Load(id.to_string()),
            None => agent::AcpOpen::Fresh,
        };
        let launch = agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                session_id: &session.id,
                branch_id: &branch.id,
                runtime: &runtime,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &session.model,
                effort: &session.effort,
                goal_file: goal_file.as_deref(),
                primer_file: primer_file.as_deref(),
                extra_env: &extra_env,
                env_clear: session.policy_env_clear,
                mode: &mode,
                prelude: &session.policy_prelude,
                restricted: session.policy_restricted,
                allowed_tools: &session.policy_allowed_tools,
                mcp_access: &session.policy_mcp_access,
                custom: custom_agent,
            },
            open,
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::acp::start(st, &session.id, launch)
            .await
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // A re-adopted ACP session is live again — mark it running.
    let status = agent::initial_status(&st.db, &session.agent_kind).await;
    session_mod::set_status(&st.db, &session.id, status).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": reason }),
    )
    .await
    .ok();
    tracing::info!(session = %session.id, branch = %branch.id, "acp session adopted");
    Ok(())
}

fn stamped_primer_file(run_dir: &std::path::Path, prelude: &str) -> Option<PathBuf> {
    if prelude != "weaver" {
        return None;
    }
    let file = run_dir.join("primer.txt");
    file.exists().then_some(file)
}

/// Resolve the persisted primer/goal files used to resume either backend. Refresh
/// the positional goal from the authoritative branch artifact first: an ACP
/// adapter that cannot load its old provider session falls back to this prompt in
/// exactly the same way as a native terminal resume.
async fn resume_prompt_files(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let run_dir = db::run_dir(&session.id);
    let primer_file = stamped_primer_file(&run_dir, &session.policy_prelude);
    let goal_file = {
        let f = run_dir.join("goal.txt");
        if f.exists() {
            match branch_mod::current_goal(&st.db, branch).await {
                Ok(goal) => {
                    if let Err(e) = tokio::fs::write(&f, &goal).await {
                        tracing::warn!(error = %e, "failed to refresh goal.txt on resume");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to read goal for resume refresh"),
            }
            tracing::debug!(session = %session.id, "refreshed goal file for resume");
            Some(f)
        } else {
            None
        }
    };
    (primer_file, goal_file)
}

/// Re-launch a session's agent in a worktree that already exists on disk: the
/// shared tail of [`adopt`] (orphaned → resume) and [`recover`] (archived →
/// rebuild the worktree, then resume). `reason` is the status event's reason
/// string. Setup is never re-run here — the worktree is already provisioned; this
/// only resumes the agent (Claude via `--continue`, so it reloads its prior
/// conversation from the same cwd).
async fn resume_agent(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    reason: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> Result<(), AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, reason = %reason, "resuming agent");
    let work_dir = PathBuf::from(&session.work_dir);
    // Restore the persisted positional prompt and any optional system primer.
    let (primer_file, goal_file) = resume_prompt_files(st, session, branch).await;
    // Re-launch with the same layered env the session started with, so a resumed
    // session keeps its per-repo / config-file environment (not just the global
    // agent_env). Setup is NOT re-run on adopt — the worktree is already
    // provisioned; this only resumes the agent.
    let repo_root = PathBuf::from(&branch.repo_root);
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
    rotate_session_token(&st.db, session, &mut extra_env).await?;
    let runtime = session.agent_kind.clone();
    tracing::info!(session = %session.id, branch = %branch.id, runtime = %runtime, work_dir = %work_dir.display(), "relaunching agent terminal for resume");
    agent::launch(
        &st.db,
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &runtime,
            work_dir: &work_dir,
            term_session: &session.term_session,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            prelude: &session.policy_prelude,
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            extra_env: &extra_env,
            env_clear: session.policy_env_clear,
            custom: custom_agent,
        },
        agent::LaunchMode::Adopt,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::debug!(session = %session.id, "agent terminal relaunched, resuming conversation");
    // A resumed agent is already established and live — mark it `running`.
    let status = agent::initial_status(&st.db, &runtime).await;
    session_mod::set_status(&st.db, &session.id, status).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": reason }),
    )
    .await
    .ok();
    tracing::info!(session = %session.id, branch = %branch.id, reason = %reason, "session resumed");
    Ok(())
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

        tracing::debug!(session = %session.id, branch = %branch.id, protocol = %session.protocol, "resuming recovered agent");
        if session.protocol == "acp" {
            adopt_acp(
                st,
                session,
                branch,
                "session recovered",
                custom_agent.as_ref(),
            )
            .await
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
            resume_agent(
                st,
                session,
                branch,
                "session recovered",
                custom_agent.as_ref(),
            )
            .await
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
        session_mod::set_status(&st.db, &session.id, "archived").await?;
        return Err(error);
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
    recover(&st, &session, &branch).await?;
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

pub(super) async fn events_sse(
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
/// `by` (a watch name, or `manual` when absent).
pub(super) async fn send_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    // A cross-session send must not sit behind an ACP turn indefinitely. Steer
    // a supported live turn; otherwise cancel it and immediately start this
    // message as a fresh turn, while keeping the same `nudge` audit.
    if session.protocol == "acp" {
        let handle = require_acp_task(&st, &session)?;
        let by = author_or_manual(req.by.as_deref());
        let ack = handle
            .send_now(req.text.clone(), Some(by.clone()), Vec::new())
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
            "steered": ack.steered,
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
// person or watch uses — a steering/queueing send (`/prompt`), a
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
/// events (see [`crate::acp`]). A client fetches `/chat` first, then applies this
/// tail. When no task is running the stream stays open but silent (keep-alive).
pub(super) async fn chat_stream(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let boxed: Pin<Box<dyn Stream<Item = Result<sse::Event, Infallible>> + Send>> =
        match st.acp.get(&session.id) {
            Some(handle) => {
                let stream = BroadcastStream::new(handle.subscribe()).filter_map(|r| {
                    let ev = r.ok()?;
                    Some(Ok(sse::Event::default()
                        .event(ev.event)
                        .json_data(ev.data)
                        .unwrap_or_default()))
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
    #[serde(default)]
    pub force_steer: bool,
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

/// Send a user message to an ACP session: dispatched as a `session/prompt` when
/// idle, steered into a live turn when supported, or appended to the durable
/// queue otherwise. Returns 202 `{ queued, steered, turn }`. Every send records
/// a `nudge` event (the audit rule).
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
        handle
            .prompt(
                req.text.clone(),
                Some(by.clone()),
                req.force_steer,
                resources,
            )
            .await
    }
    .map_err(|e| AppError::conflict(e.to_string()))?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "nudge",
        json!({ "by": by, "text": audit_text, "promoted_queue": req.force_queued }),
    )
    .await
    .ok();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "queued": ack.queued,
            "steered": ack.steered,
            "turn": ack.turn,
        })),
    ))
}

/// Pull unseen next-turn feedback back out of the durable queue for editing.
/// The ACP task owns the consume so this action is serialized with automatic
/// dispatch at a turn boundary and with steering responses.
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
    Path((key, request_id)): Path<(String, String)>,
    Json(req): Json<PermissionBody>,
) -> ApiResult<Json<Value>> {
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
