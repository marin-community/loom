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
use crate::{
    agent, agent_env, backend, config, custom_agents, db, events, git, github, repo, repo_env,
    setup,
};
use weaver_api::{
    CreateReq, EnsureResumptionCueReq, HandoffReq, HistoryPageView, LaunchOverrides,
    LaunchSelection, PatchSessionReq, ResumptionCueView, SearchSessionsOptions, SendReq,
    SessionSearchAttention, SessionSearchStatus, SessionView, SetTagsReq, SetTitleGenerationReq,
    TagReq,
};
use weaver_core::branch as branch_mod;
use weaver_core::branch::{Branch, TitleProvenance, TitleUpdate};
use weaver_core::tags;
use weaver_core::watch::{self as watch_store, Watch};

use super::scratch::{prepare_initial_scratch, scratch_note, write_prepared_initial_scratch};
use super::{author_or_manual, require_branch, require_session, session_view};
use super::{ApiResult, AppError, AppState};

const MISSING_GITHUB_TOKEN_MESSAGE: &str = "No GitHub token configured. Add your personal GitHub token in Settings > Account, or configure a write-only GH_TOKEN on the selected profile.";
// A Unicode scalar occupies at most four UTF-8 bytes, so this character bound
// also fits the ACP transient prompt's 128 KiB byte ceiling.
const HANDOFF_SUMMARY_CHARS: usize = 32 * 1024;
const HANDOFF_RECENT_MESSAGES: usize = 8;
const HANDOFF_RECENT_CHARS: usize = 16 * 1024;
const HANDOFF_SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// External lifecycle work (terminal supervisors + git worktrees) cannot share
/// a SQLite transaction. Serialize those operations process-wide, then use
/// compare-and-set database transitions at their commit boundaries. The app
/// manages only hundreds of sessions, so a coarse lock keeps the invariant
/// legible without adding a per-session lock registry.
static LIFECYCLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn insert_session_row(
    st: &AppState,
    session: &NewSession,
    policy: &session_mod::SessionLaunchPolicy,
) -> Result<Session, AppError> {
    let (session, layout_revision) =
        session_mod::insert_with_layout_revision(&st.db, session, policy).await?;
    if let Some(revision) = layout_revision {
        super::session_layout::publish_invalidation(st, revision).await;
    }
    Ok(session)
}

async fn delete_session_row(st: &AppState, session_id: &str) -> Result<(), AppError> {
    if let Some(revision) = session_mod::delete(&st.db, session_id).await? {
        super::session_layout::publish_invalidation(st, revision).await;
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
        SessionCollectionFilter {
            archived: q.archived,
            archived_only: false,
            automation: q.automation.unwrap_or(false),
            managed: q.managed,
            search: q.q.as_deref(),
            status: None,
            attention: None,
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
        SessionCollectionFilter {
            archived: q.history || q.archived_only,
            archived_only: q.archived_only,
            automation: true,
            managed: false,
            search: Some(&q.query),
            status: q.status,
            attention: q.attention,
        },
    )
    .await
    .map(Json)
}

fn view_attention(view: &SessionView) -> &str {
    if view.status == "archived" {
        "ok"
    } else if view.branch.tags.iter().any(|tag| tag.value == "blocked") {
        "blocked"
    } else if matches!(view.status.as_str(), "error" | "orphaned")
        || view.branch.tags.iter().any(|tag| tag.value == "attention")
    {
        "attention"
    } else {
        "ok"
    }
}

fn append_search_field(haystack: &mut String, value: &str) {
    haystack.push(' ');
    haystack.push_str(value);
}

fn search_haystack(view: &SessionView) -> String {
    let mut haystack = String::new();
    if let Some(placement) = &view.placement {
        append_search_field(
            &mut haystack,
            &format!("{} / {}", placement.group_name, view.branch.title.trim()),
        );
        append_search_field(&mut haystack, &placement.group_name);
    }
    for field in [
        view.branch.title.as_str(),
        view.branch.goal.as_str(),
        view.branch.description.as_str(),
        view.github_repo.as_deref().unwrap_or_default(),
        view.branch.repo_root.as_str(),
        view.branch.branch.as_str(),
        view.branch.name.as_str(),
        view.status.as_str(),
        view.profile.as_str(),
        view.origin.as_str(),
        view.class.as_str(),
        view.created_by.as_deref().unwrap_or_default(),
        view.parent_session_id.as_deref().unwrap_or_default(),
        view.parent_id.as_deref().unwrap_or_default(),
    ] {
        append_search_field(&mut haystack, field);
    }
    if let Some(issue) = &view.github_issue {
        append_search_field(&mut haystack, &format!("{}#{}", issue.repo, issue.number));
        append_search_field(&mut haystack, &format!("#{}", issue.number));
    }
    if let Some(issue) = view.tracking_issue {
        append_search_field(&mut haystack, &format!("#{issue}"));
    }
    if let Some(pr) = &view.branch.github {
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
    } else if let Some(pr) = view.branch.github_pr {
        append_search_field(&mut haystack, &format!("#{pr}"));
    }
    for tag in &view.branch.tags {
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

struct SessionCollectionFilter<'a> {
    archived: bool,
    archived_only: bool,
    automation: bool,
    managed: bool,
    search: Option<&'a str>,
    status: Option<SessionSearchStatus>,
    attention: Option<SessionSearchAttention>,
}

async fn collect_sessions(
    st: &AppState,
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
    let needle = filter
        .search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let sessions = if filter.managed {
        session_mod::list(&st.db).await?
    } else {
        session_mod::list_visible(&st.db).await?
    };
    let mut views: Vec<SessionView> = Vec::with_capacity(sessions.len());
    for s in sessions {
        if !filter.managed && warm.contains(&s.id) {
            continue;
        }
        // Archived sessions are torn down — hidden unless the caller opts in.
        if !filter.archived && s.status == "archived" {
            continue;
        }
        if filter.archived_only && s.status != "archived" {
            continue;
        }
        // Explicit compatibility reads can still hide automation-class rows.
        if !filter.automation && s.class == "automation" {
            continue;
        }
        if let Some(branch) = branch_mod::get(&st.db, &s.branch_id).await? {
            let view = session_view(&st.db, &s, &branch).await?;
            if filter
                .status
                .is_some_and(|status| view.status != status.as_str())
            {
                continue;
            }
            if filter.attention.is_some_and(|attention| match attention {
                SessionSearchAttention::Needs => view_attention(&view) == "ok",
                SessionSearchAttention::Ok => view_attention(&view) != "ok",
                SessionSearchAttention::Attention => view_attention(&view) != "attention",
                SessionSearchAttention::Blocked => view_attention(&view) != "blocked",
            }) {
                continue;
            }
            if let Some(needle) = &needle {
                // The wire view already carries every promised search facet:
                // qualified placement, title/goal, repo/branch, issue/PR, tags,
                // status, profile, and provenance. Searching only its values
                // keeps that vocabulary synchronized without matching JSON keys.
                let hay = search_haystack(&view);
                if !hay.contains(needle) {
                    continue;
                }
            }
            views.push(view);
        }
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
    let _ = prepare_initial_scratch(&req.scratch)?;
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
    let actor = crate::runtime::Actor::from_principal(&principal, delegated);
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
    Ok(Json(crate::runtime::create_session(st, req, actor).await?))
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

/// The valid `[env]` entries from a repo's `.weaver/config.toml`, as launch
/// pairs. A name that isn't a shell identifier, or that uses loom's reserved
/// `WEAVER_`/`LOOM_` prefixes, is dropped with a warning — it would corrupt the
/// `export` or shadow the environment loom relies on (the same rule `agent_env`
/// enforces on operator vars).
fn config_env_pairs(cfg: &weaver_core::repo_config::RepoConfig) -> Vec<(String, String)> {
    cfg.env
        .iter()
        .filter(|(name, _)| match agent_env::validate_name(name) {
            Ok(()) => true,
            Err(why) => {
                tracing::warn!(name = %name, why = %why,
                    "ignoring .weaver/config.toml [env] entry");
                false
            }
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Load a repo's `.weaver/config.toml`, logging and degrading to the empty config
/// on a parse error. For the infra launch paths (warm watch session, adopt)
/// where there is no create-time request to reject: the file only supplies env
/// and defaults there, so a malformed one must not block resuming a session — but
/// it still gets logged rather than silently swallowed.
fn repo_cfg_or_default(repo_root: &std::path::Path) -> weaver_core::repo_config::RepoConfig {
    weaver_core::repo_config::load(repo_root).unwrap_or_else(|e| {
        tracing::warn!(repo = %repo_root.display(), error = %e,
            "ignoring malformed .weaver/config.toml");
        weaver_core::repo_config::RepoConfig::default()
    })
}

/// Build the environment exported into a session's agent terminal, starting
/// with the selected profile and then layering the per-repo [`repo_env`] and
/// committed `.weaver/config.toml` `[env]`. A strict profile keeps ownership of
/// its declared names; a restricted profile receives only its own environment.
/// Loom fills in its repo-local defaults last only when no layer supplied the
/// name. Best-effort: a database error in a layer degrades to the layers that
/// did resolve. `cfg` is the already-loaded repo config.
async fn launch_env_for_profile(
    db: &Db,
    repo_root: &std::path::Path,
    cfg: &weaver_core::repo_config::RepoConfig,
    profile_name: &str,
    strict: bool,
    restricted: bool,
) -> Vec<(String, String)> {
    let env = crate::profile::env_pairs(db, profile_name)
        .await
        .unwrap_or_default();
    layer_launch_environment(db, repo_root, cfg, profile_name, env, strict, restricted).await
}

async fn layer_launch_environment(
    db: &Db,
    repo_root: &std::path::Path,
    cfg: &weaver_core::repo_config::RepoConfig,
    profile_name: &str,
    mut env: Vec<(String, String)>,
    strict: bool,
    restricted: bool,
) -> Vec<(String, String)> {
    let repo_root_str = repo_root.display().to_string();
    if restricted {
        tracing::debug!(repo = %repo_root_str, profile = profile_name, "restricted launch uses profile environment only");
        return env;
    }
    let repo_pairs = repo_env::pairs(db, &repo_root_str)
        .await
        .unwrap_or_default();
    let config_pairs = config_env_pairs(cfg);
    if strict {
        // A strict profile's declared names are policy, not defaults. Repo
        // layers may add variables but cannot replace a profile-owned value.
        for (name, value) in repo_pairs.into_iter().chain(config_pairs) {
            if !env.iter().any(|(existing, _)| existing == &name) {
                env.push((name, value));
            }
        }
    } else {
        repo_env::layer(&mut env, repo_pairs);
        repo_env::layer(&mut env, config_pairs);
    }
    tracing::debug!(repo = %repo_root_str, profile = profile_name, strict, env_vars = env.len(), "layered launch environment");
    env
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
    let env = launch_env_for_profile(
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

/// Overlay the launching user's personal GitHub token onto `env` as `GH_TOKEN`,
/// so the session's `git push` / `gh` act as that user (their pushes and PRs are
/// attributed to them, matching the per-user commit identity loom already sets).
/// The user's registered token takes precedence over any `GH_TOKEN` from the
/// selected profile, repository environment, or committed repo config. Only for
/// a launch that carries a `created_by` username. Best-effort: a lookup failure
/// is logged, never fatal, so a token-store hiccup can't block a launch.
async fn apply_user_github_token(
    db: &Db,
    env: &mut Vec<(String, String)>,
    created_by: Option<&str>,
) {
    let Some(username) = created_by else { return };
    match crate::user_token::get(db, username).await {
        Ok(Some(token)) if !token.trim().is_empty() => {
            set_env(env, "GH_TOKEN", token);
            tracing::info!(%username, "applied user github token as GH_TOKEN");
        }
        Ok(_) => {
            tracing::debug!(%username, "no personal github token on file, retaining session GH_TOKEN")
        }
        Err(e) => tracing::warn!(%username, "failed to load user github token: {e}"),
    }
}

/// Set `name` in `env`, replacing an existing entry in place (so a user token
/// overrides a lower-precedence value) or appending it when absent.
fn set_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(k, _)| k == name) {
        slot.1 = value;
    } else {
        env.push((name.to_string(), value));
    }
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

fn env_has_key(env: &[(String, String)], name: &str) -> bool {
    env.iter().any(|(k, _)| k == name)
}

fn env_has_nonempty(env: &[(String, String)], name: &str) -> bool {
    env.iter().any(|(k, v)| k == name && !v.trim().is_empty())
}

fn ambient_env_has_nonempty(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

async fn ensure_github_token_available(
    db: &Db,
    env: &[(String, String)],
    created_by: Option<&str>,
    runtime: &str,
    restricted_github_app: Option<&crate::github_app::GithubApp>,
) -> ApiResult<()> {
    // Only the builtin PR-driving agents (claude/codex) need GitHub credentials to
    // push as the user. A custom agent is operator-defined — it may be a manual
    // terminal or never touch GitHub, and the operator supplies whatever
    // credentials it needs via env — so it is exempt, as the old manual "shell"
    // terminal was.
    if agent::builtin_agent_type(runtime).is_none() {
        return Ok(());
    }
    let Some(username) = created_by else {
        return Ok(());
    };
    // Webhook launches carry an attribution string, not a real approved user.
    // Their GitHub credentials come from the app/ambient path rather than a
    // per-user token row.
    if crate::auth::get_user(db, username).await?.is_none() {
        return Ok(());
    }
    if env_has_nonempty(env, "GH_TOKEN")
        || (!env_has_key(env, "GH_TOKEN") && ambient_env_has_nonempty("GH_TOKEN"))
    {
        return Ok(());
    }
    if crate::user_token::get(db, username)
        .await?
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Ok(());
    }
    // Restricted sessions do not push directly. Their fixed GitHub tools can
    // mint a repository-scoped installation token on demand; callers pass the
    // App only for that launch posture.
    if let Some(app) = restricted_github_app {
        if app.is_configured().await {
            return Ok(());
        }
    }
    tracing::warn!(created_by = ?created_by, runtime = %runtime, "launch blocked: no github token available");
    Err(AppError::new(
        StatusCode::PRECONDITION_REQUIRED,
        MISSING_GITHUB_TOKEN_MESSAGE,
    ))
}

async fn fetch_launch_issue(
    st: &AppState,
    repo_root: &std::path::Path,
    managed_repo: Option<&crate::repo::RepoSlug>,
    number: i64,
) -> anyhow::Result<github::Issue> {
    if let (Some(app), Some(repo)) = (st.trigger.app(), managed_repo) {
        if app.is_configured().await {
            return app.issue(&repo.owner, &repo.name, number).await;
        }
    }
    github::fetch_issue(repo_root, number).await
}

/// The configured wall-clock budget for a repo setup run.
async fn setup_timeout(db: &Db) -> std::time::Duration {
    let secs = config::get(db, "setup.timeout_secs")
        .await
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(config::DEFAULT_SETUP_TIMEOUT_SECS as u64)
        .max(1);
    std::time::Duration::from_secs(secs)
}

fn repo_setup_for_profile(
    cfg: &weaver_core::repo_config::RepoConfig,
    restricted: bool,
) -> Option<String> {
    (!restricted).then(|| cfg.setup.script()).flatten()
}

/// Run a registered repo's `[setup]` script in the worktree before the agent
/// starts, recording its lifecycle as `setup` events (so the session view shows
/// it) and capturing full output to `setup.log` in the run dir. The caller has
/// already confirmed the repo is allowlisted. Returns the outcome; the caller
/// decides whether to launch the agent or leave the session in an error state.
async fn run_repo_setup(
    st: &AppState,
    branch_id: &str,
    work_dir: &std::path::Path,
    run_dir: &std::path::Path,
    script: &str,
    env: &[(String, String)],
) -> setup::SetupOutcome {
    let timeout = setup_timeout(&st.db).await;
    tracing::info!(branch = branch_id, work_dir = %work_dir.display(), timeout_secs = timeout.as_secs(), "running repo [setup] script");
    events::record(
        &st.db,
        &st.bus,
        branch_id,
        "setup",
        json!({ "phase": "started", "timeout_secs": timeout.as_secs() }),
    )
    .await
    .ok();

    let log_path = run_dir.join("setup.log");
    let outcome = setup::run(work_dir, script, env, timeout, Some(&log_path))
        .await
        .unwrap_or_else(|e| setup::SetupOutcome {
            success: false,
            timed_out: false,
            exit_code: None,
            output: format!("failed to start setup: {e}"),
            duration: std::time::Duration::ZERO,
        });

    // The full output lives in setup.log; the event carries a bounded tail so the
    // timeline stays light.
    let tail = tail_chars(&outcome.output, 4000);
    events::record(
        &st.db,
        &st.bus,
        branch_id,
        "setup",
        json!({
            "phase": "finished",
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "exit_code": outcome.exit_code,
            "duration_ms": outcome.duration.as_millis() as u64,
            "summary": outcome.summary(),
            "output": tail,
        }),
    )
    .await
    .ok();
    if outcome.success {
        tracing::info!(branch = branch_id, "repo setup succeeded");
    } else {
        tracing::warn!(branch = branch_id, summary = %outcome.summary(), "repo setup failed");
    }
    outcome
}

/// The last `max` chars of `s` (whole string when shorter), prefixed with an
/// elision marker when truncated. Keeps a setup-output event payload bounded.
fn tail_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let tail: String = s
        .chars()
        .rev()
        .take(max)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…(truncated)\n{tail}")
}

fn legacy_launch_selection(req: &CreateReq) -> LaunchSelection {
    let nonempty = |value: &Option<String>| {
        value
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let selected = |value: &Option<String>| value.as_ref().map(|value| value.trim().to_string());
    LaunchSelection {
        profile: nonempty(&req.profile)
            .unwrap_or_else(|| crate::profile::DEFAULT_PROFILE.to_string()),
        overrides: LaunchOverrides {
            agent: nonempty(&req.agent),
            // Empty is an explicit "agent default" selector for these legacy
            // fields, distinct from omission (which inherits the profile).
            model: selected(&req.model),
            effort: selected(&req.effort),
            protocol: selected(&req.protocol),
            mode: nonempty(&req.mode),
            class: nonempty(&req.class),
        },
    }
}

fn create_selection(req: &CreateReq) -> ApiResult<LaunchSelection> {
    let Some(selection) = &req.selection else {
        return Ok(legacy_launch_selection(req));
    };
    let flattened = [
        &req.profile,
        &req.agent,
        &req.model,
        &req.effort,
        &req.protocol,
        &req.mode,
        &req.class,
    ]
    .into_iter()
    .any(Option::is_some);
    if flattened {
        return Err(AppError::bad_request(
            "canonical `selection` cannot be combined with flattened launch selectors",
        ));
    }
    if req.expected_profile_revision.is_none() || req.expected_resolver_revision.is_none() {
        return Err(AppError::bad_request(
            "canonical `selection` requires expected_profile_revision and expected_resolver_revision from a resolve preview",
        ));
    }
    Ok(selection.clone())
}

fn legacy_handoff_mode(requested: &Option<String>, current: &str) -> String {
    requested
        .as_deref()
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .unwrap_or(current)
        .to_string()
}

fn handoff_selection(req: &HandoffReq, session: &Session) -> ApiResult<LaunchSelection> {
    if let Some(selection) = &req.selection {
        if !req.agent.trim().is_empty()
            || req.model.is_some()
            || req.effort.is_some()
            || req.mode.is_some()
        {
            return Err(AppError::bad_request(
                "canonical handoff selection cannot be combined with flattened agent/model/effort/mode fields",
            ));
        }
        return Ok(selection.clone());
    }
    let target = req.agent.trim();
    if target.is_empty() {
        return Err(AppError::bad_request("handoff agent is required"));
    }
    Ok(LaunchSelection {
        profile: session.profile.clone(),
        overrides: LaunchOverrides {
            agent: Some(target.to_string()),
            model: req.model.clone(),
            effort: req.effort.clone(),
            // A flattened handoff historically retained the live session's
            // permission posture when mode was absent or blank.
            mode: Some(legacy_handoff_mode(&req.mode, &session.launch_mode)),
            ..Default::default()
        },
    })
}

fn handoff_resolve_options(session: &Session) -> crate::launch::ResolveOptions {
    crate::launch::ResolveOptions {
        // Resolve the selected template's real class. The handoff boundary
        // compares it with the existing session instead of coercing it first.
        default_class: None,
        capacity_credit_profile: crate::profile::status_consumes_capacity(&session.status)
            .then(|| session.profile.clone()),
        ..Default::default()
    }
}

async fn resolve_handoff_selection(
    st: &AppState,
    session: &Session,
    selection: &LaunchSelection,
) -> ApiResult<crate::launch::ResolvedLaunch> {
    let mut resolved =
        super::launches::resolve_launch(st, selection, &handoff_resolve_options(session)).await?;
    if resolved.view.class != session.class {
        resolved.view.errors.push(format!(
            "profile '{}' is {}-class; this {} session cannot change class during handoff",
            resolved.profile.name, resolved.view.class, session.class
        ));
    }
    if resolved.view.protocol != "acp" {
        resolved.view.errors.push(format!(
            "agent '{}' does not resolve to the ACP protocol required for handoff",
            resolved.view.agent
        ));
    }
    if resolved.profile.restricted {
        resolved
            .view
            .errors
            .push("restricted profiles cannot be applied by handoff".to_string());
    }
    resolved.view.valid = resolved.view.errors.is_empty();
    Ok(resolved)
}

fn require_handoff_source(session: &Session) -> ApiResult<()> {
    require_acp(session)?;
    if session.policy_restricted {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "restricted sessions cannot change agent runtime",
        ));
    }
    if !matches!(session.status.as_str(), "running" | "orphaned" | "error") {
        return Err(AppError::conflict(format!(
            "session '{}' is {}, not handoff-capable",
            session.id, session.status
        )));
    }
    if session.managed_by.is_some() {
        return Err(AppError::conflict(
            "engine-managed sessions cannot be handed off manually",
        ));
    }
    Ok(())
}

pub(super) async fn resolve_session_handoff(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<weaver_api::ResolveLaunchReq>,
) -> ApiResult<Json<weaver_api::ResolvedLaunchView>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_handoff_source(&session)?;
    let profile_name = match req.selection.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(profile_name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    Ok(Json(
        resolve_handoff_selection(&st, &session, &req.selection)
            .await?
            .view,
    ))
}

/// The session-creation core shared by every producer. The actor supplies trusted
/// attribution, origin, ancestry, and profile bounds; request fields cannot
/// impersonate those properties. Returns the view directly so each caller can
/// shape its own response.
pub(crate) async fn provision_session(
    st: AppState,
    req: CreateReq,
    actor: crate::runtime::Actor,
) -> ApiResult<SessionView> {
    let created_by = actor.display_creator();
    let origin = actor.origin();
    tracing::info!(
        repo = ?req.repo,
        agent = ?req.agent,
        created_by = ?created_by,
        origin,
        "provision_session: starting session creation"
    );
    // Attachment input is untrusted launch input, not a provisioning step.
    // Decode and validate the entire batch before touching a repository,
    // worktree, branch, tracking issue, claim, or session row.
    let prepared_scratch = prepare_initial_scratch(&req.scratch)?;
    let selection = create_selection(&req)?;
    let selected_profile_name = match selection.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    }
    .to_string();
    // This is both the capacity gate and the template-lifetime gate. Profile
    // edits, retirement, recreation, clone, and environment mutation use the
    // same permit; retaining it through session insertion closes the
    // resolve/provision/delete gap even for unlimited profiles.
    let _profile_permit = st.launch_gate.acquire_profile(&selected_profile_name).await;
    // Custom-agent and custom-MCP mutations share this boundary. Keep the
    // exact registry generation accepted below through command construction
    // and runtime startup.
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let options = crate::launch::ResolveOptions {
        default_class: matches!(origin, "watch" | "actions" | "ops")
            .then(|| "automation".to_string()),
        ..Default::default()
    };
    let resolved = match super::launches::resolve_launch(&st, &selection, &options).await {
        Ok(resolved) => resolved,
        Err(error) if req.expected_resolver_revision.is_some() => {
            return Err(AppError::conflict(format!(
                "launch settings can no longer be resolved after preview: {}",
                error.message()
            )));
        }
        Err(error) => return Err(error),
    };
    let stale_profile = req
        .expected_profile_revision
        .is_some_and(|expected| expected != resolved.view.profile_revision);
    let stale_resolver = req
        .expected_resolver_revision
        .as_deref()
        .is_some_and(|expected| expected != resolved.view.resolver_revision);
    if stale_profile || stale_resolver {
        return Err(AppError::conflict(
            "launch settings changed since preview; review the fresh resolution",
        )
        .with_fields(json!({ "preview": resolved.view })));
    }
    if !resolved.view.valid {
        let message = resolved
            .view
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| "launch settings are not valid".to_string());
        let status = if resolved.view.capacity.allowed {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::CONFLICT
        };
        return Err(AppError::new(status, message).with_fields(json!({ "preview": resolved.view })));
    }
    // Resolve write-only values once, then confirm the template revision still
    // matches. Environment mutations advance that revision transactionally, so
    // provisioning below uses this concrete snapshot instead of silently
    // reading a value changed after preview.
    let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let current_profile = crate::profile::get(&st.db, &resolved.profile.name)
        .await?
        .ok_or_else(|| AppError::bad_request("selected profile was removed after preview"))?;
    if current_profile.revision != resolved.view.profile_revision
        || current_profile.lifetime != resolved.view.profile_lifetime
    {
        let fresh = super::launches::resolve_launch(&st, &selection, &options).await?;
        return Err(AppError::conflict(
            "launch profile changed while resolving its environment; review the fresh resolution",
        )
        .with_fields(json!({ "preview": fresh.view })));
    }
    let profile_name = resolved.profile.name.clone();
    let custom_agent = resolved.custom_agent.clone();
    let launch_profile = resolved.profile;
    let agent = resolved.view.agent.clone();
    let model = resolved.view.model.clone();
    let effort = resolved.view.effort.clone();
    let protocol = resolved.view.protocol.clone();
    let mode = resolved.view.mode.clone();
    let class = resolved.view.class.clone();
    let launch_snapshot =
        crate::launch::serialize_snapshot(&resolved.view, resolved.custom_agent.as_ref())
            .map_err(|error| AppError::bad_request(error.to_string()))?;
    if let Some(allowed) = actor.allowed_profiles() {
        if !allowed.iter().any(|name| name == &profile_name) {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                format!("automation grant does not allow profile '{profile_name}'"),
            ));
        }
        if !launch_profile.is_automation_safe() {
            return Err(AppError::bad_request(format!(
                "automation profile '{profile_name}' must be automation-class, strict, and env-cleared"
            )));
        }
    }
    let managed_repo = req.repo.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // Resolve the stable repository identity without mutating it, then hold its
    // launch permit through clone/fetch, worktree setup, and agent startup. A
    // different repository gets a different permit and remains independent.
    let repo_key = match managed_repo {
        Some(input) => repo::registered_path(&st.db, input)
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
    let repo_key = repo_key.canonicalize().unwrap_or(repo_key);
    tracing::debug!(repo = %repo_key.display(), "waiting for repository launch gate");
    let launch_permit = st.launch_gate.acquire(&repo_key).await;
    tracing::debug!(repo = %repo_key.display(), "acquired repository launch gate");

    // Recheck capacity while holding the profile-wide admission gate. The
    // permit remains live through session insertion, so launches against
    // different repositories cannot over-admit the same profile.
    if launch_profile.max_concurrent > 0
        && crate::profile::active_count(&st.db, &profile_name).await?
            >= launch_profile.max_concurrent
    {
        let fresh = super::launches::resolve_launch(&st, &selection, &options).await?;
        return Err(AppError::conflict(format!(
            "profile '{profile_name}' has reached its max_concurrent limit ({})",
            launch_profile.max_concurrent
        ))
        .with_fields(json!({ "preview": fresh.view })));
    }

    // Now acquire the managed clone (inside the gate), or reuse the local root
    // resolved above. The traversal / allowlist boundary lives in `repo`.
    let repo_root = match managed_repo {
        Some(input) => repo::resolve_clone(&st.db, input, st.trigger.app())
            .await
            .map_err(|e| match e {
                repo::ResolveError::BadRequest(m) => AppError::bad_request(m),
                repo::ResolveError::Clone(m) => AppError::new(StatusCode::BAD_GATEWAY, m),
            })?,
        None => repo_key,
    };
    // Canonicalize so repo identity matches the `weaver` CLI's resolver — issues
    // are keyed on this path and the two binaries must agree on it.
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);
    tracing::debug!(repo_root = %repo_root.display(), "resolved repo root");

    // The repo's committed `.weaver/config.toml`, read from its primary checkout.
    // It supplies agent/model/effort defaults (below an explicit request, above
    // the operator's global default), the `[env]` layer exported into the
    // terminal, and the `[setup]` bootstrap run for allowlisted repos. A malformed
    // file is a hard error *only* for an allowlisted repo (whose setup would run),
    // so the breakage is visible at create time; for any other repo it would have
    // supplied mere defaults, so we log and proceed with an empty config.
    let repo_cfg = match weaver_core::repo_config::load(&repo_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            if repo::is_allowlisted(&st.db, &repo_root)
                .await
                .unwrap_or(false)
            {
                return Err(AppError::bad_request(format!(
                    "repo .weaver/config.toml is invalid: {e}"
                )));
            }
            tracing::warn!(repo = %repo_root.display(), error = %e,
                "ignoring malformed .weaver/config.toml");
            weaver_core::repo_config::RepoConfig::default()
        }
    };
    tracing::debug!(repo_root = %repo_root.display(), "loaded repo config");

    // A GitHub App token is repository-scoped. A managed slug gives both the
    // preflight and issue seeding an exact installation target; a local path
    // must keep using an explicitly supplied session credential.
    let managed_slug = req
        .repo
        .as_deref()
        .and_then(|repo| crate::repo::parse_slug(repo).ok());
    let runtime = agent.clone();
    tracing::debug!(agent = %agent, runtime = %runtime, "resolved agent runtime");
    // The resolved launch environment: selected profile < per-repo repo_env <
    // the repo file's [env]. It is needed before provisioning so a real agent
    // launch can stop cleanly when neither the user nor an environment layer
    // provides GH_TOKEN.
    let mut extra_env = layer_launch_environment(
        &st.db,
        &repo_root,
        &repo_cfg,
        &profile_name,
        profile_environment,
        launch_profile.strict,
        launch_profile.restricted,
    )
    .await;
    if launch_profile.env_clear {
        let allowlist = launch_profile
            .ambient_names()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }
    // Select the launching user's GitHub credential by overlaying it as
    // GH_TOKEN (design §6.3, "Level B"). See `apply_user_github_token` for the
    // precedence rules. This happens before preflight. Ordinary sessions export
    // it; a restricted ACP launch removes it from the adapter environment and
    // its server-side GitHub tool independently resolves an App or profile
    // credential.
    apply_user_github_token(&st.db, &mut extra_env, created_by.as_deref()).await;

    tracing::debug!(model = %model, effort = %effort, protocol = %protocol, "resolved and validated model/effort/protocol");
    let restricted_github_app = if launch_profile.restricted && managed_slug.is_some() {
        st.trigger.app()
    } else {
        None
    };
    ensure_github_token_available(
        &st.db,
        &extra_env,
        created_by.as_deref(),
        &runtime,
        restricted_github_app,
    )
    .await?;
    tracing::debug!(runtime = %runtime, "github token availability check passed");

    // Build title/goal/description; an optional GitHub issue seeds all three.
    let mut goal = req.goal.unwrap_or_default().trim().to_string();
    let mut title = req
        .title
        .as_deref()
        .and_then(branch_mod::sanitize_user_title);
    let title_was_explicit = title.is_some();
    let mut description = String::new();
    let mut github_repo = None;
    let mut github_issue: Option<i64> = None;
    if let Some(number) = req.issue {
        tracing::info!(issue = number, repo = %repo_root.display(), "fetching github issue to seed session");
        let issue = fetch_launch_issue(&st, &repo_root, managed_slug.as_ref(), number)
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
        github_repo = match managed_slug.as_ref() {
            Some(repo) => Some(repo.slug()),
            None => github::repo_slug(&repo_root).await.ok(),
        };
        tracing::debug!(issue = number, github_repo = ?github_repo, "seeded session fields from github issue");
    } else if let Some(number) = req.github_issue {
        // The caller already holds the thread (the `@loom` trigger): record the
        // GitHub link on the tracking issue without the fetch-and-seed above.
        github_issue = Some(number);
        github_repo = managed_slug.as_ref().map(|repo| repo.slug());
    }

    // Claiming an existing weaver issue seeds the same three fields from it.
    let repo_root_str = repo_root.display().to_string();
    let mut claimed_issue_id: Option<i64> = None;
    if let Some(issue_id) = req.claim_issue {
        tracing::debug!(issue_id, "claiming existing weaver issue for new session");
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
    let issue_owned_title =
        req.issue.is_some() || req.github_issue.is_some() || req.claim_issue.is_some();
    let title_provenance = if title_was_explicit {
        if issue_owned_title && origin == "github" {
            TitleProvenance::Issue
        } else {
            TitleProvenance::User
        }
    } else if issue_owned_title {
        TitleProvenance::Issue
    } else {
        TitleProvenance::Derived
    };
    let mut title = title
        .and_then(|title| branch_mod::sanitize_user_title(&title))
        .unwrap_or_else(|| branch_mod::derive_title(&goal));

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
    tracing::debug!(base = %base, "resolved base branch");

    let (branch_name, work_dir) = if let Some(existing_branch) = existing {
        tracing::info!(branch = %existing_branch, "reusing existing branch for session");
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
            Some(p) => {
                tracing::debug!(branch = %existing_branch, work_dir = %p.display(), "found existing worktree for branch");
                p
            }
            None => {
                let slug = branch_mod::slugify(existing_branch);
                let dir = repo_root.join(".worktrees").join(&slug);
                tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
                git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
                tracing::info!(branch = %existing_branch, work_dir = %dir.display(), "provisioning worktree for existing branch");
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
        tracing::debug!(base_slug = %base_slug, base = %base, "creating new branch for session");
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
        tracing::info!(branch = %branch_name, work_dir = %work_dir.display(), base = %base, "provisioning worktree for new branch");
        git::worktree_add(&repo_root, &work_dir, &branch_name, &base)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        (branch_name, work_dir)
    };

    // A replacement launch may reuse a worktree whose previous session left
    // Scratch files behind. Validate and write the merged set while holding the
    // same path-scoped permit as live upload/delete routes, before creating any
    // branch row, tracking issue, claim, or session.
    let _scratch_permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let scratch_names = write_prepared_initial_scratch(&work_dir, &prepared_scratch).await?;

    // Get-or-create the branch row, then stamp its title/goal/description.
    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    tracing::debug!(branch = %branch.id, branch_name = %branch_name, "upserted branch row");
    // A plain relaunch of an existing branch resumes its human-owned identity;
    // only new task input intentionally replaces it.
    let preserve_existing_title = existing.is_some()
        && !title_was_explicit
        && !issue_owned_title
        && goal.is_empty()
        && !branch.title.is_empty();
    if preserve_existing_title {
        title.clone_from(&branch.title);
    } else {
        branch_mod::set_title(&st.db, &branch.id, &title, title_provenance).await?;
    }
    if !goal.is_empty() {
        branch_mod::set_goal(&st.db, &branch.id, &goal, "user").await?;
    }
    if !description.is_empty() {
        branch_mod::set_description(&st.db, &branch.id, &description).await?;
    }
    // Re-fetch so the view we return reflects the freshly-stamped fields.
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "branch vanished"))?;
    tracing::debug!(branch = %branch.id, title = %title, "stamped branch title/goal/description");

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
    let parent_session_id = match &parent {
        Some(parent) => session_mod::active_for_branch(&st.db, &parent.id)
            .await?
            .map(|session| session.id),
        None => None,
    };
    let stamped_allowed_tools = serde_json::to_string(&resolved.runtime_permissions)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let stamped_mcp_access = serde_json::to_string(&resolved.mcp_policy)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let (creator_kind, creator_subject) = actor.creator_identity();
    let launch_policy = session_mod::SessionLaunchPolicy {
        profile: profile_name.clone(),
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
        creator_kind: creator_kind.to_string(),
        creator_subject,
        parent_session_id,
        automation_run_id: actor.automation_run_id().map(str::to_string),
    };

    // Open this session's tracking issue before the launch prompt is written,
    // so the agent can be told its issue number. When an agent delegated this
    // work (`parent_branch`), the parent becomes the issue's `source_branch`.
    tracing::debug!(branch = %branch.id, "opening tracking issue for session");
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
    tracing::debug!(branch = %branch.id, tracking_issue = ?tracking_issue, "tracking issue resolved");

    // Automation reserves its session id durably before provisioning. Reusing
    // it here makes retries converge on one runtime identity instead of
    // silently allocating a second session after an ambiguous response.
    let session_id = actor
        .reserved_session_id()
        .map(str::to_string)
        .unwrap_or_else(branch_mod::new_id);
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    tracing::info!(session = %session_id, branch = %branch.id, run_dir = %run_dir.display(), "allocated session id and run dir");

    // Drop any attached reference files into the worktree before the agent
    // launches, then tell the agent they are there. The branch goal stays the
    // clean text the user typed; the scratch and tracking notes ride on the
    // launch prompt (goal.txt) only, so they reach the agent without cluttering
    // the dashboard.
    tracing::debug!(session = %session_id, scratch_files = scratch_names.len(), "wrote initial scratch files");
    // Goal, scratch, and tracking context ride in as the positional prompt that
    // seeds the session's first turn.
    let goal_file = {
        let scratch = scratch_note(&scratch_names);
        let entrance = entrance_note(tracking_issue);
        let launch_prompt = build_launch_prompt(
            &goal,
            &launch_profile.prelude,
            &entrance,
            scratch.as_deref(),
        );
        if launch_prompt.is_empty() {
            None
        } else {
            let f = run_dir.join("goal.txt");
            tokio::fs::write(&f, &launch_prompt).await?;
            tracing::debug!(session = %session_id, "wrote goal file for launch prompt");
            Some(f)
        }
    };

    let term_session = format!("weaver-{session_id}");
    tracing::debug!(session = %session_id, term_session = %term_session, "derived terminal session name");

    // Attribute the agent's commits to the launching user (design §6.3, Level A):
    // export their GitHub identity as the git author/committer. Inserted only if
    // not already set by a preceding env layer, so an explicit repo/operator
    // override still wins, and only for an interactive launch that carries a
    // `created_by` principal (webhook/warm/adopt paths have none and keep the
    // shared identity).
    if let Some(username) = created_by.as_deref() {
        match crate::auth::commit_identity(&st.db, username).await {
            Ok(Some(id)) => {
                for (k, v) in [
                    ("GIT_AUTHOR_NAME", &id.name),
                    ("GIT_AUTHOR_EMAIL", &id.email),
                    ("GIT_COMMITTER_NAME", &id.name),
                    ("GIT_COMMITTER_EMAIL", &id.email),
                ] {
                    if !extra_env.iter().any(|(ek, _)| ek == k) {
                        extra_env.push((k.to_string(), v.clone()));
                    }
                }
                tracing::debug!(%username, "attributed commits to launching user");
            }
            Ok(None) => {
                tracing::debug!(%username, "no commit identity registered, using shared identity")
            }
            Err(e) => tracing::warn!(%username, "failed to resolve commit identity: {e}"),
        }
    }

    // Per-repo setup: run the repo's committed `[setup]` script in the worktree
    // before the agent starts — but ONLY for an allowlisted (registered) repo,
    // because a setup script is arbitrary, privileged code (it runs with the
    // shared container's credentials; design §6.4). A non-allowlisted repo's
    // script is never executed (recorded as skipped); a failed run leaves the
    // session in a visible error state instead of launching a half-provisioned
    // worktree.
    if let Some(script) = repo_setup_for_profile(&repo_cfg, launch_profile.restricted) {
        tracing::debug!(branch = %branch.id, repo = %repo_root.display(), "repo declares a [setup] script");
        if repo::is_allowlisted(&st.db, &repo_root)
            .await
            .unwrap_or(false)
        {
            let outcome =
                run_repo_setup(&st, &branch.id, &work_dir, &run_dir, &script, &extra_env).await;
            if !outcome.success {
                tracing::warn!(branch = %branch.id, "repo setup failed, aborting launch before agent start");
                // Surface the failure as a loud, visible session state rather than
                // launching the agent into a half-provisioned worktree. The
                // worktree is left intact for inspection; full output is in the
                // run dir's setup.log.
                let session = insert_session_row(
                    &st,
                    &NewSession {
                        id: session_id.clone(),
                        branch_id: branch.id.clone(),
                        work_dir: work_dir.display().to_string(),
                        term_session: term_session.clone(),
                        agent_kind: agent.clone(),
                        model: model.clone(),
                        effort: effort.clone(),
                        status: "error".to_string(),
                        github_repo: github_repo.clone(),
                        parent_branch_id: parent.as_ref().map(|b| b.id.clone()),
                        managed_by: None,
                        created_by: created_by.clone(),
                        protocol: protocol.clone(),
                        origin: origin.to_string(),
                        class: class.clone(),
                        tracking_issue_id: tracking_issue,
                    },
                    &launch_policy,
                )
                .await?;
                tracing::info!(
                    branch = %branch.id,
                    session = %session.id,
                    status = %session.status,
                    agent = %session.agent_kind,
                    "session created"
                );
                let note = outcome.summary();
                tags::set(
                    &st.db,
                    &branch.id,
                    tags::ATTENTION_KEY,
                    "blocked",
                    &note,
                    "loom",
                )
                .await
                .ok();
                events::record_tag(
                    &st.db,
                    &st.bus,
                    &branch.id,
                    tags::ATTENTION_KEY,
                    "blocked",
                    &note,
                    "loom",
                )
                .await
                .ok();
                events::record(
                    &st.db,
                    &st.bus,
                    &branch.id,
                    "status",
                    json!({ "status": "error", "reason": "repo setup failed" }),
                )
                .await
                .ok();
                return session_view(&st.db, &session, &branch).await;
            }
        } else {
            tracing::info!(repo = %repo_root.display(),
                "skipping .weaver/config.toml [setup]: repo is not allowlisted");
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "setup",
                json!({ "phase": "skipped", "reason": "repo not allowlisted" }),
            )
            .await
            .ok();
        }
    }

    // Live the moment the agent spawns — there is no `launching` state.
    let status = agent::initial_status(&st.db, &runtime).await;
    let new_session = NewSession {
        id: session_id.clone(),
        branch_id: branch.id.clone(),
        work_dir: work_dir.display().to_string(),
        term_session: term_session.clone(),
        agent_kind: agent.clone(),
        model: model.clone(),
        effort: effort.clone(),
        status: status.to_string(),
        github_repo: github_repo.clone(),
        parent_branch_id: parent.as_ref().map(|b| b.id.clone()),
        managed_by: None,
        created_by: created_by.clone(),
        protocol: protocol.clone(),
        origin: origin.to_string(),
        class: class.clone(),
        tracking_issue_id: tracking_issue,
    };
    crate::auth::revoke_session_tokens(&st.db, &session_id).await?;
    let session_token =
        crate::auth::create_session_token(&st.db, created_by.as_deref(), &session_id, &branch.id)
            .await?;
    set_env(&mut extra_env, "LOOM_TOKEN", session_token);
    let session = if protocol == "acp" {
        // The ACP path inserts the row *first* — `acp::start` binds a relay to it
        // and reads it back — then brings up the headless adapter over the relay.
        tracing::info!(
            session = %session_id, branch = %branch.id, runtime = %runtime,
            work_dir = %work_dir.display(), mode = %mode, "launching acp session"
        );
        let session = insert_session_row(&st, &new_session, &launch_policy).await?;
        // A custom ACP agent supplies the exact adapter command accepted by
        // canonical resolution; registry edits after preview cannot replace it.
        let launch = agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                session_id: &session.id,
                branch_id: &branch.id,
                runtime: &runtime,
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
                mcp_access: &launch_policy.mcp_access,
                custom: custom_agent.as_ref(),
            },
            agent::AcpOpen::Fresh,
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Err(e) = crate::acp::start(&st, &session.id, launch).await {
            crate::auth::revoke_session_tokens(&st.db, &session.id)
                .await
                .ok();
            // Keep the durable row/worktree visible and recoverable, but retain
            // non-2xx create semantics so CLI/webhook callers never announce a
            // failed agent as successfully launched. The browser uses the
            // returned session id to navigate to its handoff controls.
            let _ = session_mod::set_status(&st.db, &session.id, "error").await;
            let note =
                format!("Agent failed to start: {e}. Hand off this session to another provider.");
            tags::set(
                &st.db,
                &branch.id,
                tags::ATTENTION_KEY,
                "blocked",
                &note,
                "loom",
            )
            .await
            .ok();
            events::record_tag(
                &st.db,
                &st.bus,
                &branch.id,
                tags::ATTENTION_KEY,
                "blocked",
                &note,
                "loom",
            )
            .await
            .ok();
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "status",
                json!({ "status": "error", "reason": "acp launch failed", "error": e.to_string() }),
            )
            .await
            .ok();
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, format!("acp launch failed: {e}"))
                    .with_fields(json!({ "session_id": session.id })),
            );
        }
        tracing::info!(session = %session.id, branch = %branch.id, "acp session launched");
        session
    } else {
        tracing::info!(
            session = %session_id,
            branch = %branch.id,
            runtime = %runtime,
            work_dir = %work_dir.display(),
            env_vars = extra_env.len(),
            "launching agent terminal"
        );
        // Make the session-bound token resolvable before the child starts. A
        // terminal agent may call back into loom as soon as its shell execs;
        // inserting after `agent::launch` left a race where that first request
        // saw a correctly minted token as unauthorized.
        let session = insert_session_row(&st, &new_session, &launch_policy).await?;
        if let Err(e) = agent::launch(
            &st.db,
            &agent::LaunchSpec {
                branch_id: &branch.id,
                runtime: &runtime,
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
        {
            crate::auth::revoke_session_tokens(&st.db, &session_id)
                .await
                .ok();
            let _ = session_mod::set_status(&st.db, &session_id, "error").await;
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
            ));
        }
        tracing::info!(session = %session_id, branch = %branch.id, "agent terminal launched");
        session
    };
    tracing::debug!(session = %session.id, status = %status, "inserted session row");
    // The next launch may begin once this agent is live. The remaining writes
    // are bookkeeping and do not touch repository or worktree state.
    drop(launch_permit);

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

    crate::metadata_assist::spawn_title_generation(
        st.db.clone(),
        st.bus.clone(),
        session.clone(),
        branch.clone(),
        false,
    )
    .await
    .ok();
    session_view(&st.db, &session, &branch).await
}

/// Session-specific operating context appended after the goal. Keep this
/// compact: the goal is the outcome, the primer owns durable workflow rules,
/// and this note only supplies the tracking contract that neither can know
/// ahead of time. `weaver summary` is a recovery path, not a mandatory first
/// tool call that would inject the goal a second time.
fn entrance_note(tracking_issue: Option<i64>) -> String {
    let mut note = "You are working in a Weaver session. Use `weaver summary` \
                    to recover context if needed and `weaver readme` for the \
                    complete workflow guide."
        .to_string();
    if let Some(id) = tracking_issue {
        note.push_str(&format!(
            " This session is tracked as weaver issue #{id}: keep `weaver \
             status <level> \"<message>\"` honest as you work, and run `weaver \
             issue close {id}` once the task is complete (e.g. the PR is open) \
             so whoever launched you knows you are done."
        ));
    }
    note
}

/// Construct the positional first prompt from the stamped prelude policy.
/// The user's goal is always the opening user message: making an agent fetch it
/// through `weaver summary` on turn one adds latency and duplicates the goal in
/// context. `none` deliberately omits all Weaver orientation.
fn build_launch_prompt(goal: &str, prelude: &str, entrance: &str, scratch: Option<&str>) -> String {
    let mut parts = Vec::new();
    if !goal.is_empty() {
        parts.push(goal);
        if prelude == "weaver" {
            parts.push(entrance);
        }
    }
    if let Some(scratch) = scratch {
        parts.push(scratch);
    }
    parts.join("\n\n")
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
    tracing::debug!(branch = %branch.id, source = %source, "resolving tracking issue for session");

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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
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
            TitleUpdate::Applied(_) => {}
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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        // A competing remove already achieved the requested end state.
        return Ok(Vec::new());
    };
    remove_locked(st, &current_session, &current_branch, keep_branch).await
}

/// Shared deletion after the caller has acquired [`LIFECYCLE_LOCK`] and
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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
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

/// Shared teardown after the caller has acquired [`LIFECYCLE_LOCK`] and
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
    let session = insert_session_row(
        st,
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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
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
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
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

// Session log, conversation, and event-stream endpoints.

pub(super) async fn log_session(
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
    let log = crate::chatlog::conversation(&st.db, &session, &branch)
        .await
        .ok_or_else(|| AppError::not_found("conversation"))?;
    // A terminal transcript can be many megabytes. JSON serialization is CPU
    // work too, so keep it beside discovery/parsing on the blocking pool rather
    // than letting a large response stall unrelated async routes.
    let body = tokio::task::spawn_blocking(move || serde_json::to_vec(&log)).await??;
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
        let blocks = crate::chat::list(&st.db, &session.id).await?;
        let n = if q.lines == 0 { 40 } else { q.lines };
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

struct HandoffPlan {
    target: String,
    model: String,
    effort: String,
    mode: String,
    profile: String,
    profile_revision: i64,
    profile_lifetime: i64,
    env_clear: bool,
    ambient_allowlist: String,
    idle_archive_secs: Option<i64>,
    turn_budget: i64,
    prelude: String,
    restricted: bool,
    strict: bool,
    allowed_tools: String,
    mcp_access: String,
    launch_snapshot: String,
    profile_environment: Vec<(String, String)>,
    custom_agent: Option<custom_agents::CustomAgent>,
}

fn legacy_handoff_snapshot(
    session: &Session,
    target: &str,
    model: &str,
    effort: &str,
    mode: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> ApiResult<String> {
    if session.launch_snapshot.trim().is_empty() {
        return Ok(String::new());
    }
    let mut snapshot = crate::launch::deserialize_snapshot(&session.launch_snapshot)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    snapshot.view.agent = target.to_string();
    snapshot.view.model = model.to_string();
    snapshot.view.effort = effort.to_string();
    snapshot.view.protocol = "acp".to_string();
    snapshot.view.mode = mode.to_string();
    snapshot.custom_agent = custom_agent.cloned();
    snapshot.view.selection.overrides.agent = Some(target.to_string());
    snapshot.view.selection.overrides.model = Some(model.to_string());
    snapshot.view.selection.overrides.effort = Some(effort.to_string());
    snapshot.view.selection.overrides.mode = Some(mode.to_string());
    snapshot.view.provenance.agent = "launch_override".to_string();
    snapshot.view.provenance.model = if model.is_empty() {
        "agent_default"
    } else {
        "launch_override"
    }
    .to_string();
    snapshot.view.provenance.effort = if effort.is_empty() {
        "agent_default"
    } else {
        "launch_override"
    }
    .to_string();
    snapshot.view.provenance.protocol = "agent_default".to_string();
    snapshot.view.provenance.mode = "launch_override".to_string();
    crate::launch::serialize_snapshot(&snapshot.view, snapshot.custom_agent.as_ref())
        .map_err(|error| AppError::bad_request(error.to_string()))
}

async fn legacy_handoff_plan(
    st: &AppState,
    req: &HandoffReq,
    session: &Session,
) -> ApiResult<HandoffPlan> {
    let target = req.agent.trim();
    if target.is_empty() {
        return Err(AppError::bad_request("handoff agent is required"));
    }
    let custom_agent = if crate::agent::builtin_agent_type(target).is_some() {
        None
    } else {
        Some(
            custom_agents::get(&st.db, target)
                .await?
                .ok_or_else(|| AppError::bad_request(format!("unknown agent '{target}'")))?,
        )
    };
    let metadata = match custom_agent.as_ref() {
        Some(custom) => crate::agent::custom_metadata(custom),
        None => crate::agent::metadata_for(&st.db, target)
            .await?
            .ok_or_else(|| AppError::bad_request(format!("unknown agent '{target}'")))?,
    };
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let effort = req
        .effort
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    crate::agent::validate_model(&metadata, &model).map_err(AppError::bad_request)?;
    crate::agent::validate_effort(&metadata, &effort).map_err(AppError::bad_request)?;
    let protocol =
        crate::agent::resolve_protocol(&metadata, None).map_err(AppError::bad_request)?;
    if protocol != "acp" {
        return Err(AppError::bad_request(format!(
            "agent '{target}' does not resolve to the ACP protocol required for handoff"
        )));
    }
    let mode = legacy_handoff_mode(&req.mode, &session.launch_mode);
    if !matches!(
        mode.as_str(),
        "auto" | "default" | "acceptEdits" | "plan" | "bypassPermissions"
    ) {
        return Err(AppError::bad_request(format!(
            "invalid handoff mode '{mode}'"
        )));
    }
    let lifetime = crate::profile::get_including_retired(&st.db, &session.profile)
        .await?
        .ok_or_else(|| {
            AppError::conflict(
                "the session's original profile lifetime is unavailable; review a canonical handoff preview",
            )
        })?;
    if session.profile_lifetime == 0 || lifetime.lifetime != session.profile_lifetime {
        return Err(AppError::conflict(
            "the session's profile name now refers to a different template lifetime; review a canonical handoff preview",
        ));
    }
    let keeps_same_slot = crate::profile::status_consumes_capacity(&session.status);
    if lifetime.max_concurrent > 0
        && !keeps_same_slot
        && crate::profile::active_count(&st.db, &session.profile).await? >= lifetime.max_concurrent
    {
        return Err(AppError::conflict(format!(
            "profile '{}' has reached its max_concurrent limit ({})",
            session.profile, lifetime.max_concurrent
        )));
    }
    let profile_environment = crate::profile::env_pairs(&st.db, &session.profile)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    Ok(HandoffPlan {
        target: target.to_string(),
        model: model.clone(),
        effort: effort.clone(),
        mode: mode.clone(),
        profile: session.profile.clone(),
        profile_revision: session.profile_revision,
        profile_lifetime: session.profile_lifetime,
        env_clear: session.policy_env_clear,
        ambient_allowlist: session.policy_ambient_allowlist.clone(),
        idle_archive_secs: session.policy_idle_archive_secs,
        turn_budget: session.policy_turn_budget,
        prelude: session.policy_prelude.clone(),
        restricted: session.policy_restricted,
        strict: session.policy_strict,
        allowed_tools: session.policy_allowed_tools.clone(),
        mcp_access: session.policy_mcp_access.clone(),
        launch_snapshot: legacy_handoff_snapshot(
            session,
            target,
            &model,
            &effort,
            &mode,
            custom_agent.as_ref(),
        )?,
        profile_environment,
        custom_agent,
    })
}

/// Replace the provider behind an idle ACP work session while preserving loom's
/// stable session/branch/worktree identity and canonical journal.
pub(super) async fn handoff_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<HandoffReq>,
) -> ApiResult<Json<SessionView>> {
    let (initial_session, _) = require_session(&st.db, &key).await?;
    require_handoff_source(&initial_session)?;
    let canonical = req.selection.is_some();
    if canonical
        && (req.expected_profile_revision.is_none() || req.expected_resolver_revision.is_none())
    {
        return Err(AppError::bad_request(
            "canonical handoff selection requires expected_profile_revision and expected_resolver_revision from a handoff preview",
        ));
    }
    let _source_permit = st.launch_gate.acquire_session(&initial_session.id).await;
    let _lifecycle = LIFECYCLE_LOCK.lock().await;
    let Some((session, branch)) = session_mod::with_branch(&st.db, &initial_session.id).await?
    else {
        return Err(AppError::conflict(
            "session changed while the handoff request was waiting; review it again",
        ));
    };
    require_handoff_source(&session)?;
    let unchanged_source = session.status == initial_session.status
        && session.agent_kind == initial_session.agent_kind
        && session.model == initial_session.model
        && session.effort == initial_session.effort
        && session.profile == initial_session.profile
        && session.profile_revision == initial_session.profile_revision
        && session.profile_lifetime == initial_session.profile_lifetime
        && session.launch_mode == initial_session.launch_mode
        && session.launch_snapshot == initial_session.launch_snapshot
        && session.mutation_revision == initial_session.mutation_revision;
    if !unchanged_source {
        return Err(AppError::conflict(
            "session changed while the handoff request was waiting; review it again",
        ));
    }
    let selection = canonical
        .then(|| handoff_selection(&req, &session))
        .transpose()?;
    let permit_profile = selection
        .as_ref()
        .map(|selection| match selection.profile.trim() {
            "" => crate::profile::DEFAULT_PROFILE,
            name => name,
        })
        .unwrap_or(session.profile.as_str())
        .to_string();
    let _profile_permit = st.launch_gate.acquire_profile(&permit_profile).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let plan = if let Some(selection) = selection {
        let resolved = match resolve_handoff_selection(&st, &session, &selection).await {
            Ok(resolved) => resolved,
            Err(error) if req.expected_resolver_revision.is_some() => {
                return Err(AppError::conflict(format!(
                    "handoff settings can no longer be resolved after preview: {}",
                    error.message()
                )));
            }
            Err(error) => return Err(error),
        };
        if req
            .expected_profile_revision
            .is_some_and(|expected| expected != resolved.view.profile_revision)
            || req
                .expected_resolver_revision
                .as_deref()
                .is_some_and(|expected| expected != resolved.view.resolver_revision)
        {
            return Err(AppError::conflict(
                "handoff settings changed after preview; review the fresh resolution",
            )
            .with_fields(json!({ "preview": resolved.view })));
        }
        if !resolved.view.valid {
            return Err(AppError::conflict(
                "resolved handoff settings are not currently launchable",
            )
            .with_fields(json!({ "preview": resolved.view })));
        }
        let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?;
        let launch_snapshot =
            crate::launch::serialize_snapshot(&resolved.view, resolved.custom_agent.as_ref())
                .map_err(|error| AppError::bad_request(error.to_string()))?;
        HandoffPlan {
            target: resolved.view.agent.clone(),
            model: resolved.view.model.clone(),
            effort: resolved.view.effort.clone(),
            mode: resolved.view.mode.clone(),
            profile: resolved.profile.name.clone(),
            profile_revision: resolved.profile.revision,
            profile_lifetime: resolved.profile.lifetime,
            env_clear: resolved.profile.env_clear,
            ambient_allowlist: resolved.profile.ambient_allowlist.clone(),
            idle_archive_secs: resolved.view.policy.idle_archive_secs,
            turn_budget: resolved.view.policy.turn_budget.unwrap_or(0),
            prelude: resolved.profile.prelude.clone(),
            restricted: resolved.profile.restricted,
            strict: resolved.profile.strict,
            allowed_tools: serde_json::to_string(&resolved.runtime_permissions)
                .map_err(|error| AppError::bad_request(error.to_string()))?,
            mcp_access: serde_json::to_string(&resolved.mcp_policy)
                .map_err(|error| AppError::bad_request(error.to_string()))?,
            launch_snapshot,
            profile_environment,
            custom_agent: resolved.custom_agent,
        }
    } else {
        legacy_handoff_plan(&st, &req, &session).await?
    };
    let target = plan.target.clone();
    let model = plan.model.clone();
    let effort = plan.effort.clone();
    let mode = plan.mode.clone();
    if target == session.agent_kind
        && model == session.model
        && effort == session.effort
        && plan.profile == session.profile
        && plan.profile_revision == session.profile_revision
        && mode == session.launch_mode
    {
        return Err(AppError::bad_request(
            "handoff target matches the current runtime profile",
        ));
    }
    let handoff_policy = session_mod::SessionHandoffPolicy {
        agent_kind: target.clone(),
        model: model.clone(),
        effort: effort.clone(),
        profile: plan.profile.clone(),
        launch_mode: mode.clone(),
        profile_revision: plan.profile_revision,
        profile_lifetime: plan.profile_lifetime,
        strict: plan.strict,
        env_clear: plan.env_clear,
        ambient_allowlist: plan.ambient_allowlist.clone(),
        idle_archive_secs: plan.idle_archive_secs,
        turn_budget: plan.turn_budget,
        prelude: plan.prelude.clone(),
        restricted: plan.restricted,
        allowed_tools: plan.allowed_tools.clone(),
        mcp_access: plan.mcp_access.clone(),
        launch_snapshot: plan.launch_snapshot.clone(),
    };

    // Resolve every fallible launch input before quiescing the current task.
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(AppError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot hand off",
            session.work_dir
        )));
    }
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = layer_launch_environment(
        &st.db,
        &repo_root,
        &repo_cfg,
        &plan.profile,
        plan.profile_environment.clone(),
        plan.strict,
        plan.restricted,
    )
    .await;
    if plan.env_clear {
        let allowlist: Vec<String> = serde_json::from_str(&plan.ambient_allowlist)
            .map_err(|error| AppError::bad_request(error.to_string()))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }
    apply_user_github_token(&st.db, &mut extra_env, session.created_by.as_deref()).await;
    // Restricted sessions return before this handoff path, so an App credential
    // cannot be relevant here.
    ensure_github_token_available(
        &st.db,
        &extra_env,
        session.created_by.as_deref(),
        &target,
        None,
    )
    .await?;
    // A healthy task quiesces on its ordered command channel, preserving the
    // active-turn/queue safety gate. A missing task is the recovery case: settle
    // its persisted in-flight turn, retain the durable queue, and continue.
    let snapshot = if let Some(handle) = st.acp.get(&session.id) {
        match handle.prepare_handoff().await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tokio::task::yield_now().await;
                if st.acp.is_live(&session.id) {
                    return Err(AppError::conflict(error.to_string()));
                }
                tracing::warn!(session = %session.id, %error,
                    "ACP task vanished while preparing handoff; using persisted recovery state");
                None
            }
        }
    } else {
        tracing::warn!(session = %session.id,
            "handing off without a live ACP task; using persisted recovery state");
        None
    };
    let source_task_quiesced = snapshot.is_some();
    // Re-read after the task handshake: it may have vanished after our initial
    // route snapshot while persisting a newer in-flight turn.
    let persisted = session_mod::get(&st.db, &session.id)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?;
    if let Some(turn) = session_mod::acp_inflight_turn(&persisted) {
        crate::chat::close_abandoned_turn(&st.db, &session.id, turn).await?;
    }
    let blocks = match snapshot {
        Some(blocks) => blocks,
        None => crate::chat::list(&st.db, &session.id).await?,
    };
    let current_goal = branch_mod::current_goal(&st.db, &branch).await?;
    let context = crate::chat::handoff_context(
        &current_goal,
        &blocks,
        HANDOFF_SUMMARY_CHARS,
        HANDOFF_RECENT_MESSAGES,
        HANDOFF_RECENT_CHARS,
    );
    // Only after the source provider has accepted the handoff preflight do we
    // mint a replacement credential. The old credential remains valid until
    // the replacement policy commits; every failure below revokes only this
    // staged token.
    let staged_token = crate::auth::stage_session_token(
        &st.db,
        session.created_by.as_deref(),
        &session.id,
        &session.branch_id,
    )
    .await?;
    set_env(&mut extra_env, "LOOM_TOKEN", staged_token.value.clone());
    let mut launch = match agent::build_acp_launch(
        &st.db,
        &agent::AcpLaunchSpec {
            session_id: &session.id,
            branch_id: &branch.id,
            runtime: &target,
            work_dir: &work_dir,
            server_addr: &st.addr,
            model: &model,
            effort: &effort,
            goal_file: None,
            primer_file: None,
            extra_env: &extra_env,
            env_clear: plan.env_clear,
            mode: &mode,
            prelude: &plan.prelude,
            restricted: plan.restricted,
            allowed_tools: &handoff_policy.allowed_tools,
            mcp_access: &handoff_policy.mcp_access,
            custom: plan.custom_agent.as_ref(),
        },
        agent::AcpOpen::Fresh,
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
                .await
                .ok();
            if source_task_quiesced {
                crate::acp::attach(&st, &session.id).await.ok();
            }
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            ));
        }
    };
    let digest = agent::AgentManager::new(&st.db)
        .summarize_handoff(
            &target,
            &context.summary_request,
            &launch,
            HANDOFF_SUMMARY_TIMEOUT,
        )
        .await;
    launch.goal = Some(crate::chat::handoff_prompt(
        &current_goal,
        digest.text.as_deref(),
        &context.recent_dialogue,
    ));
    // The source may emit its final idle lifecycle edge while acknowledging
    // preflight. It is quiesced now, so fence provider replacement against this
    // post-handshake generation rather than the route's earlier snapshot.
    let claimed_generation = persisted.mutation_revision + 1;
    let Some(source_state) =
        session_mod::claim_handoff(&st.db, &session.id, persisted.mutation_revision).await?
    else {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        if source_task_quiesced {
            let current = session_mod::get(&st.db, &session.id).await?;
            if current
                .as_ref()
                .is_some_and(|current| !session_mod::is_terminal(&current.status))
            {
                crate::acp::attach(&st, &session.id).await.map_err(|error| {
                    AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "session changed before provider replacement, and its source task could not be restored: {error}"
                        ),
                    )
                })?;
            }
        }
        return Err(AppError::conflict(
            "session changed before the handoff could replace its provider; review it again",
        ));
    };
    if let Err(kill_error) = backend::kill_session_and_wait(&session.term_session).await {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        if backend::has_session(&session.term_session).await {
            match session_mod::rollback_handoff_claim(
                &st.db,
                &session.id,
                claimed_generation,
                &source_state,
            )
            .await?
            {
                Some(restored_generation) => {
                    if let Err(attach_error) = crate::acp::attach(&st, &session.id).await {
                        session_mod::fail_handoff_claim(&st.db, &session.id, restored_generation)
                            .await
                            .ok();
                        return Err(AppError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!(
                                "source provider teardown failed ({kill_error}); durable state was restored but reattach failed ({attach_error}), so the session was marked error"
                            ),
                        ));
                    }
                    return Err(AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "source provider teardown failed; the original provider was restored: {kill_error}"
                        ),
                    ));
                }
                None => {
                    return Err(AppError::conflict(
                        "session changed while failed handoff teardown was rolling back; the newer state was preserved",
                    ));
                }
            }
        }
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation)
            .await
            .ok();
        return Err(AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "source provider teardown failed after the provider disappeared; the session was marked recoverable error: {kill_error}"
            ),
        ));
    }
    if !session_mod::clear_claimed_handoff_source(&st.db, &session.id, claimed_generation).await? {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        return Err(AppError::conflict(
            "session changed after source teardown; the newer state was preserved",
        ));
    }
    if let Err(error) = crate::chat::reset_usage(&st.db, &session.id).await {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation)
            .await
            .ok();
        return Err(error.into());
    }

    let boundary = json!({
        "from": session.agent_kind,
        "to": target,
        "model": model,
        "effort": effort,
        "prompt_version": crate::chat::HANDOFF_PROMPT_VERSION,
        "summary_status": digest.status,
        "summary_model": digest.model,
        "summary": digest.text,
        "through_turn": context.through.map(|(turn, _)| turn),
        "through_seq": context.through.map(|(_, seq)| seq),
    });
    if let Err(e) = crate::acp::start_handoff(&st, &session.id, launch, boundary).await {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        let failure_committed = session_mod::prepare_handoff(
            &st.db,
            &session.id,
            "error",
            &handoff_policy,
            claimed_generation,
        )
        .await
        .unwrap_or(false);
        if failure_committed {
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "status",
                json!({ "status": "error", "reason": "agent handoff failed" }),
            )
            .await
            .ok();
        }
        return Err(AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("agent handoff failed: {e}"),
        ));
    }
    if !session_mod::prepare_handoff(
        &st.db,
        &session.id,
        "running",
        &handoff_policy,
        claimed_generation,
    )
    .await?
    {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        return Err(AppError::conflict(
            "session changed while the replacement provider was starting; the newer state was preserved",
        ));
    }
    if let Err(error) =
        crate::auth::commit_staged_session_token(&st.db, &session.id, &staged_token.id).await
    {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation + 1)
            .await
            .ok();
        return Err(AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("replacement provider token could not be committed: {error}"),
        ));
    }

    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "handoff",
        json!({ "from": session.agent_kind, "to": target, "model": model, "effort": effort }),
    )
    .await
    .ok();
    let (session, branch) = require_session(&st.db, &session.id).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_create_preserves_explicit_agent_default_selectors() {
        let req = CreateReq {
            profile: Some(" template ".to_string()),
            agent: Some(" ".to_string()),
            model: Some(" ".to_string()),
            effort: Some(String::new()),
            protocol: Some(" ".to_string()),
            mode: Some(" ".to_string()),
            class: Some(" ".to_string()),
            ..Default::default()
        };
        let selection = legacy_launch_selection(&req);
        assert_eq!(selection.profile, "template");
        assert_eq!(selection.overrides.agent, None);
        assert_eq!(selection.overrides.model.as_deref(), Some(""));
        assert_eq!(selection.overrides.effort.as_deref(), Some(""));
        assert_eq!(selection.overrides.protocol.as_deref(), Some(""));
        assert_eq!(selection.overrides.mode, None);
        assert_eq!(selection.overrides.class, None);
    }

    #[test]
    fn legacy_handoff_inherits_current_mode_when_omitted_or_blank() {
        assert_eq!(legacy_handoff_mode(&None, "acceptEdits"), "acceptEdits");
        assert_eq!(
            legacy_handoff_mode(&Some(" ".to_string()), "acceptEdits"),
            "acceptEdits"
        );
        assert_eq!(
            legacy_handoff_mode(&Some(" plan ".to_string()), "acceptEdits"),
            "plan"
        );
    }

    async fn seed_user(db: &Db, username: &str) {
        sqlx::query("INSERT INTO users (username) VALUES (?)")
            .bind(username)
            .execute(db)
            .await
            .unwrap();
    }

    struct EnvVarGuard {
        name: &'static str,
        value: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn unset(name: &'static str) -> Self {
            let value = std::env::var_os(name);
            std::env::remove_var(name);
            Self { name, value }
        }

        fn set(name: &'static str, new_value: &str) -> Self {
            let value = std::env::var_os(name);
            std::env::set_var(name, new_value);
            Self { name, value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.value {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[tokio::test]
    async fn user_github_token_injected_as_gh_token() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();

        let mut env = vec![("FOO".to_string(), "bar".to_string())];
        apply_user_github_token(&db, &mut env, Some("alice")).await;
        assert!(
            env.iter().any(|(k, v)| k == "GH_TOKEN" && v == "ghp_alice"),
            "the launching user's token is exported as GH_TOKEN"
        );
    }

    #[tokio::test]
    async fn restricted_environment_uses_only_profile_values() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::profile::env_set(&db, "github_comment", "GH_TOKEN", "profile-token")
            .await
            .unwrap();
        crate::repo_env::set(&db, &repo.display().to_string(), "REPO_SECRET", "leak")
            .await
            .unwrap();
        let mut cfg = weaver_core::repo_config::RepoConfig::default();
        cfg.env
            .insert("COMMITTED_SECRET".to_string(), "leak".to_string());

        let env = launch_env_for_profile(&db, repo, &cfg, "github_comment", true, true).await;

        assert_eq!(
            env,
            vec![("GH_TOKEN".to_string(), "profile-token".to_string())]
        );
    }

    #[tokio::test]
    async fn stamped_strict_environment_keeps_profile_first_precedence() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::profile::env_set(&db, "default", "SHARED_TOKEN", "profile")
            .await
            .unwrap();
        crate::repo_env::set(
            &db,
            &repo.display().to_string(),
            "SHARED_TOKEN",
            "repository",
        )
        .await
        .unwrap();
        let mut cfg = weaver_core::repo_config::RepoConfig::default();
        cfg.env
            .insert("SHARED_TOKEN".to_string(), "committed".to_string());

        let env = launch_env_for_profile(&db, repo, &cfg, "default", true, false).await;

        assert_eq!(
            env.iter()
                .find(|(name, _)| name == "SHARED_TOKEN")
                .map(|(_, value)| value.as_str()),
            Some("profile")
        );
    }

    #[tokio::test]
    async fn ordinary_environment_keeps_cargo_target_worktree_local() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = weaver_core::repo_config::RepoConfig::default();

        let env = launch_env_for_profile(&db, dir.path(), &cfg, "default", false, false).await;

        assert!(!env.iter().any(|(name, _)| name == "CARGO_TARGET_DIR"));
    }

    #[test]
    fn restricted_profile_ignores_repository_setup() {
        let mut cfg = weaver_core::repo_config::RepoConfig::default();
        cfg.setup.script = Some("touch should-not-run".to_string());
        assert_eq!(
            repo_setup_for_profile(&cfg, false).as_deref(),
            Some("touch should-not-run")
        );
        assert!(repo_setup_for_profile(&cfg, true).is_none());
    }

    #[tokio::test]
    async fn user_token_overrides_ambient_gh_token_layer() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();

        // A lower env layer (the ambient Settings → Environment value, repo_env, …)
        // already set GH_TOKEN: the user's own token overrides it *in place* — so
        // their push/comment act as them — with no duplicate entry appended.
        let mut env = vec![("GH_TOKEN".to_string(), "ambient-token".to_string())];
        apply_user_github_token(&db, &mut env, Some("alice")).await;
        let gh: Vec<&(String, String)> = env.iter().filter(|(k, _)| k == "GH_TOKEN").collect();
        assert_eq!(gh.len(), 1, "no duplicate GH_TOKEN is appended");
        assert_eq!(
            gh[0].1, "ghp_alice",
            "the user's own token wins over the ambient layer"
        );
    }

    #[tokio::test]
    async fn ambient_gh_token_is_the_fallback_without_a_user_token() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "bob").await; // bob has no stored token

        // With no user token, whatever a lower layer set stands as the fallback.
        let mut env = vec![("GH_TOKEN".to_string(), "ambient-token".to_string())];
        apply_user_github_token(&db, &mut env, Some("bob")).await;
        let gh: Vec<&(String, String)> = env.iter().filter(|(k, _)| k == "GH_TOKEN").collect();
        assert_eq!(gh.len(), 1, "the ambient layer is left untouched");
        assert_eq!(
            gh[0].1, "ambient-token",
            "with no user token, the ambient value is the fallback"
        );
    }

    #[tokio::test]
    async fn gh_token_untouched_without_token_or_principal() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        // A user with no token set → nothing injected.
        let mut env = vec![("FOO".to_string(), "bar".to_string())];
        apply_user_github_token(&db, &mut env, Some("alice")).await;
        assert!(!env.iter().any(|(k, _)| k == "GH_TOKEN"));

        // A launch with no `created_by` (webhook/warm) → nothing injected, even
        // though a token now exists.
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();
        let mut env2 = vec![("FOO".to_string(), "bar".to_string())];
        apply_user_github_token(&db, &mut env2, None).await;
        assert!(!env2.iter().any(|(k, _)| k == "GH_TOKEN"));
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn real_agent_requires_github_token_for_known_user() {
        let _env = EnvVarGuard::unset("GH_TOKEN");
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        let err = ensure_github_token_available(
            &db,
            &[("FOO".to_string(), "bar".to_string())],
            Some("alice"),
            "codex",
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(err.message(), MISSING_GITHUB_TOKEN_MESSAGE);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn real_agent_accepts_user_or_default_github_token() {
        let _env = EnvVarGuard::unset("GH_TOKEN");
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();
        ensure_github_token_available(&db, &[], Some("alice"), "claude", None)
            .await
            .unwrap();

        crate::user_token::remove(&db, "alice").await.unwrap();
        ensure_github_token_available(
            &db,
            &[("GH_TOKEN".to_string(), "ghp_shared".to_string())],
            Some("alice"),
            "codex",
            None,
        )
        .await
        .unwrap();
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn empty_configured_gh_token_does_not_fall_back_to_ambient() {
        let _env = EnvVarGuard::set("GH_TOKEN", "ghp_ambient");
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        let err = ensure_github_token_available(
            &db,
            &[("GH_TOKEN".to_string(), " ".to_string())],
            Some("alice"),
            "codex",
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::PRECONDITION_REQUIRED);
    }

    #[tokio::test]
    async fn custom_and_webhook_launches_do_not_require_user_github_token() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        // A custom (non-builtin) agent is exempt — it may never touch GitHub, and
        // the operator supplies any credentials it needs via env.
        ensure_github_token_available(&db, &[], Some("alice"), "my-custom-agent", None)
            .await
            .unwrap();
        // A webhook launch carries an attribution string, not an approved user.
        ensure_github_token_available(&db, &[], Some("github-webhook (octo)"), "codex", None)
            .await
            .unwrap();
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn restricted_builtin_accepts_configured_github_app() {
        let _env = EnvVarGuard::unset("GH_TOKEN");
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        weaver_core::config::apply(
            &db,
            &[
                (
                    crate::github_app::APP_ID_KEY.to_string(),
                    Some("123456".to_string()),
                ),
                (
                    crate::github_app::APP_PRIVATE_KEY_KEY.to_string(),
                    Some("configured-for-preflight".to_string()),
                ),
            ],
        )
        .await
        .unwrap();
        let app = crate::github_app::GithubApp::new(db.clone());

        let err = ensure_github_token_available(&db, &[], Some("alice"), "claude", None)
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::PRECONDITION_REQUIRED);

        ensure_github_token_available(&db, &[], Some("alice"), "claude", Some(&app))
            .await
            .unwrap();
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
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
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
    fn entrance_note_keeps_catch_up_out_of_the_first_turn() {
        let note = entrance_note(Some(42));
        assert!(note.contains("weaver summary"));
        assert!(note.contains("recover context"));
        assert!(!note.contains("summary` first"));
        assert!(!note.contains("prints the full goal"));
        // It tells the agent exactly how to signal "done".
        assert!(note.contains("weaver issue #42"));
        assert!(note.contains("weaver issue close 42"));
        assert!(note.contains("weaver status"));
        // Untracked sessions get the orientation with no issue contract.
        let untracked = entrance_note(None);
        assert!(untracked.contains("weaver summary"));
        assert!(!untracked.contains("issue"));
    }

    #[test]
    fn restricted_prelude_delivers_the_caller_goal_without_weaver_orientation() {
        let goal = "Rewrite only the issue body.\nBody hash: abc123";
        let entrance = "weaver session metadata";
        assert_eq!(build_launch_prompt(goal, "none", entrance, None), goal);
        assert_eq!(
            build_launch_prompt(goal, "weaver", entrance, None),
            format!("{goal}\n\n{entrance}")
        );
    }
}
