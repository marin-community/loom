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
use weaver_api::{CreateReq, PatchSessionReq, SendReq, SessionView, TagReq};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::tags;
use weaver_core::watch::{self as watch_store, Watch};

use super::scratch::{scratch_note, write_initial_scratch};
use super::{author_or_manual, require_branch, require_session, session_view};
use super::{ApiResult, AppError, AppState};

const MISSING_GITHUB_TOKEN_MESSAGE: &str = "No GitHub token configured. Add your personal GitHub token in Settings > Account, or configure GH_TOKEN in Settings > Environment.";

pub(super) async fn list_agents(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let default_agent = configured_agent(&st.db, "agent.default", config::DEFAULT_AGENT).await;
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

pub(super) async fn list_sessions(
    State(st): State<AppState>,
    Query(q): Query<ListSessionsQuery>,
) -> ApiResult<Json<Vec<SessionView>>> {
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
    Json(req): Json<CreateReq>,
) -> ApiResult<Json<SessionView>> {
    // Naming a managed repo here registers it: a signed-in principal asking to
    // launch into `owner/name` is the grant, so a repo loom has never seen just
    // works (it is cloned on the way through `create_session_core`). The `repos`
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
    Ok(Json(
        create_session_core(st, req, Some(principal.username)).await?,
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
    if agent::exists(db, value).await {
        value.to_string()
    } else {
        default.to_string()
    }
}

/// Whether `value` is a valid model selector (or effort, when `model` is false)
/// for `runtime`'s agent type. A custom agent offers no selectors (empty choice
/// lists), so any non-empty value is invalid for it — as is an unknown runtime.
async fn selector_valid(db: &Db, runtime: &str, value: &str, model: bool) -> bool {
    match agent::metadata_for(db, runtime).await {
        Ok(Some(meta)) if model => agent::validate_model(&meta, value).is_ok(),
        Ok(Some(meta)) => agent::validate_effort(&meta, value).is_ok(),
        _ => false,
    }
}

async fn configured_selector(db: &Db, key: &str, runtime: &str, model: bool) -> String {
    let value = config::get_or(db, key, "").await;
    let value = value.trim();
    if value.is_empty() || !selector_valid(db, runtime, value, model).await {
        return String::new();
    }
    value.to_string()
}

/// The default agent kind for a new session when the request doesn't pin one: the
/// repo's `.weaver/config.toml` `[agent] default` when it names a real agent
/// type, else the operator's global `agent.default`. Repo-file values resolve
/// over the builtin default, mirroring `WEAVER.md`.
async fn repo_default_agent(db: &Db, cfg: &weaver_core::repo_config::RepoConfig) -> String {
    if let Some(kind) = cfg
        .agent
        .default
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        if agent::exists(db, kind).await {
            return kind.to_string();
        }
    }
    configured_agent(db, "agent.default", config::DEFAULT_AGENT).await
}

/// The model/effort selector for a new session when the request doesn't pin one:
/// the repo file's `[agent]` value when it validates for the runtime, else the
/// operator's configured default. `repo_value` is `cfg.agent.model`/`.effort`.
async fn repo_or_configured_selector(
    db: &Db,
    repo_value: Option<&str>,
    key: &str,
    runtime: &str,
    model: bool,
) -> String {
    if let Some(value) = repo_value.map(str::trim).filter(|v| !v.is_empty()) {
        if selector_valid(db, runtime, value, model).await {
            return value.to_string();
        }
    }
    configured_selector(db, key, runtime, model).await
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

/// Build the environment exported into a session's agent terminal, layered in
/// priority order: the operator's global [`agent_env`], then the per-repo
/// [`repo_env`], then the repo's committed `.weaver/config.toml` `[env]` — each
/// layer overriding the previous for a shared name. Best-effort: a database error
/// in a layer degrades to the layers that did resolve. `cfg` is the already-loaded
/// repo config (the caller reads it once for env, setup, and defaults).
async fn launch_env(
    db: &Db,
    repo_root: &std::path::Path,
    cfg: &weaver_core::repo_config::RepoConfig,
) -> Vec<(String, String)> {
    let repo_root_str = repo_root.display().to_string();
    let mut env = agent_env::pairs(db).await.unwrap_or_default();
    repo_env::layer(
        &mut env,
        repo_env::pairs(db, &repo_root_str)
            .await
            .unwrap_or_default(),
    );
    repo_env::layer(&mut env, config_env_pairs(cfg));
    tracing::debug!(repo = %repo_root_str, env_vars = env.len(), "layered launch environment");
    env
}

/// Overlay the launching user's personal GitHub token onto `env` as `GH_TOKEN`,
/// so the session's `git push` / `gh` act as that user (their pushes and PRs are
/// attributed to them, matching the per-user commit identity loom already sets).
/// The user's registered token takes precedence over any ambient `GH_TOKEN`; when
/// they have none, whatever a lower env layer set (the ambient Settings →
/// Environment value, `repo_env`, or the repo file) stands as the fallback. Only
/// for a launch that carries a `created_by` username. Best-effort: a lookup
/// failure is logged, never fatal, so a token-store hiccup can't block a launch.
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
            tracing::debug!(%username, "no personal github token on file, leaving ambient GH_TOKEN")
        }
        Err(e) => tracing::warn!(%username, "failed to load user github token: {e}"),
    }
}

/// Set `name` in `env`, replacing an existing entry in place (so a user token
/// overrides an ambient value) or appending it when absent.
fn set_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(k, _)| k == name) {
        slot.1 = value;
    } else {
        env.push((name.to_string(), value));
    }
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
    tracing::warn!(created_by = ?created_by, runtime = %runtime, "launch blocked: no github token available");
    Err(AppError::new(
        StatusCode::PRECONDITION_REQUIRED,
        MISSING_GITHUB_TOKEN_MESSAGE,
    ))
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

/// The session-creation core, shared by `POST /api/sessions` and the Chat
/// surface's concierge get-or-create ([`get_chat`]). Returns the view directly so
/// the caller can shape its own response.
///
/// `created_by` is the launching principal's username (attribution for the shared
/// board), or `None` for a system launch with no user behind it (the concierge).
pub(crate) async fn create_session_core(
    st: AppState,
    req: CreateReq,
    created_by: Option<String>,
) -> ApiResult<SessionView> {
    tracing::info!(
        repo = ?req.repo,
        agent = ?req.agent,
        created_by = ?created_by,
        "create_session_core: starting session creation"
    );
    // Resolve the repo root. An explicit managed `repo` (a slug/URL) is
    // allowlist-checked and cloned-if-absent into the managed store, then used
    // directly; otherwise fork from `cwd`'s repo (the default). The traversal /
    // allowlist gate lives in `repo::resolve_clone`.
    let repo_root = match req.repo.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(input) => repo::resolve_clone(&st.db, input, st.trigger.app())
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

    let agent = match req.agent {
        Some(a) => a.trim().to_string(),
        None => repo_default_agent(&st.db, &repo_cfg).await,
    };
    // The concierge is the fleet Chat agent, not a workstream: it gets the
    // fleet-ops primer as its opening prompt and no tracking issue (it has no
    // deliverable to track), and is hidden from the fleet list by its kind.
    let is_concierge = agent == agent::CONCIERGE_KIND;
    let runtime = launch_runtime(&st.db, &agent).await;
    tracing::debug!(agent = %agent, runtime = %runtime, is_concierge, "resolved agent runtime");
    // The resolved launch environment: global agent_env < per-repo repo_env < the
    // repo file's [env]. It is needed before provisioning so a real agent launch
    // can stop cleanly when neither the user nor the deployment provides GH_TOKEN.
    let mut extra_env = launch_env(&st.db, &repo_root, &repo_cfg).await;
    // Run the launching user's git/gh as themselves: overlay their personal
    // GitHub token as GH_TOKEN (design §6.3, "Level B"). See
    // `apply_user_github_token` for the precedence rules. This happens before
    // the preflight below so the guard and the eventual launch inspect the same
    // environment vector.
    apply_user_github_token(&st.db, &mut extra_env, created_by.as_deref()).await;

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
        None => {
            repo_or_configured_selector(
                &st.db,
                repo_cfg.agent.model.as_deref(),
                configured_model_key,
                &runtime,
                true,
            )
            .await
        }
    };
    let effort = match req.effort.as_deref().map(str::trim) {
        Some(effort) if !effort.is_empty() => effort.to_string(),
        Some(_) => String::new(),
        None => {
            repo_or_configured_selector(
                &st.db,
                repo_cfg.agent.effort.as_deref(),
                configured_effort_key,
                &runtime,
                false,
            )
            .await
        }
    };
    // Resolve the execution backend (terminal|acp) from the agent's declared
    // protocol and the optional request override, validating model/effort against
    // the same metadata. Stamped on the session row at insert, immutable after.
    let protocol = match agent::metadata_for(&st.db, &runtime).await? {
        // Blank model/effort means the agent's own default; a non-empty value must
        // be one the agent offers. A custom agent offers none, so any explicit
        // selector is rejected here.
        Some(meta) => {
            agent::validate_model(&meta, &model).map_err(AppError::bad_request)?;
            agent::validate_effort(&meta, &effort).map_err(AppError::bad_request)?;
            agent::resolve_protocol(&meta, req.protocol.as_deref())
                .map_err(AppError::bad_request)?
        }
        None => return Err(AppError::bad_request(format!("unknown agent '{runtime}'"))),
    };
    // The ACP launch permission posture (ignored for a terminal launch): the
    // request's mode, else the bypass default (the retired pre-seeded gates).
    let mode = req
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(agent::DEFAULT_ACP_MODE)
        .to_string();
    tracing::debug!(model = %model, effort = %effort, protocol = %protocol, "resolved and validated model/effort/protocol");
    ensure_github_token_available(&st.db, &extra_env, created_by.as_deref(), &runtime).await?;
    tracing::debug!(runtime = %runtime, "github token availability check passed");

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
        tracing::info!(issue = number, repo = %repo_root.display(), "fetching github issue to seed session");
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
        tracing::debug!(issue = number, github_repo = ?github_repo, "seeded session fields from github issue");
    } else if let Some(number) = req.github_issue {
        // The caller already holds the thread (the `@loom` trigger): record the
        // GitHub link on the tracking issue without the fetch-and-seed above.
        github_issue = Some(number);
        github_repo = req
            .repo
            .as_deref()
            .and_then(|r| crate::repo::parse_slug(r).ok())
            .map(|s| s.slug());
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

    // Get-or-create the branch row, then stamp its title/goal/description.
    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    tracing::debug!(branch = %branch.id, branch_name = %branch_name, "upserted branch row");
    branch_mod::set_title(&st.db, &branch.id, &title).await?;
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

    // Open this session's tracking issue before the launch prompt is written,
    // so the agent can be told its issue number. When an agent delegated this
    // work (`parent_branch`), the parent becomes the issue's `source_branch`.
    let tracking_issue = if is_concierge {
        None
    } else {
        tracing::debug!(branch = %branch.id, "opening tracking issue for session");
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
    tracing::debug!(branch = %branch.id, tracking_issue = ?tracking_issue, "tracking issue resolved");

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    tracing::info!(session = %session_id, branch = %branch.id, run_dir = %run_dir.display(), "allocated session id and run dir");

    // Drop any attached reference files into the worktree before the agent
    // launches, then tell the agent they are there. The branch goal stays the
    // clean text the user typed; the scratch and tracking notes ride on the
    // launch prompt (goal.txt) only, so they reach the agent without cluttering
    // the dashboard.
    let scratch_names = write_initial_scratch(&work_dir, &req.scratch).await?;
    tracing::debug!(session = %session_id, scratch_files = scratch_names.len(), "wrote initial scratch files");
    // The concierge boots primed-but-idle: its fleet-ops primer is injected as
    // system *context* (primer.txt → `--append-system-prompt-file`), not as a
    // positional opening prompt, so it takes no turn until the operator sends the
    // first message. A normal session's goal/scratch/tracking note ride in as the
    // positional prompt (goal.txt) that seeds its first turn.
    let (goal_file, primer_file) = if is_concierge {
        let f = run_dir.join("primer.txt");
        tokio::fs::write(&f, agent::concierge_primer()).await?;
        tracing::debug!(session = %session_id, "wrote concierge primer file");
        (None, Some(f))
    } else {
        let mut prompt_parts: Vec<String> = Vec::new();
        if !goal.is_empty() {
            // A hookless runtime (Codex, custom) never receives the WEAVER.md
            // primer, so the launch prompt is its entire orientation — those
            // still get the goal pasted. Hook-capable agents get a pointer
            // instead: the goal lives once, as the `goal` artifact.
            let hookless = !agent::metadata_for(&st.db, &runtime)
                .await
                .ok()
                .flatten()
                .is_some_and(|m| m.supports_hooks);
            if hookless {
                prompt_parts.push(goal.clone());
            }
            prompt_parts.push(entrance_note(&title, tracking_issue));
        }
        if let Some(note) = scratch_note(&scratch_names) {
            prompt_parts.push(note);
        }
        let launch_prompt = prompt_parts.join("\n\n");
        let goal_file = if launch_prompt.is_empty() {
            None
        } else {
            let f = run_dir.join("goal.txt");
            tokio::fs::write(&f, &launch_prompt).await?;
            tracing::debug!(session = %session_id, "wrote goal file for launch prompt");
            Some(f)
        };
        (goal_file, None)
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
    if let Some(script) = repo_cfg.setup.script() {
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
                let session = session_mod::insert(
                    &st.db,
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
                    },
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
                let mut view = session_view(&st.db, &session, &branch).await?;
                view.tracking_issue = tracking_issue;
                return Ok(view);
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
    };
    let session = if protocol == "acp" {
        // The ACP path inserts the row *first* — `acp::start` binds a relay to it
        // and reads it back — then brings up the headless adapter over the relay.
        tracing::info!(
            session = %session_id, branch = %branch.id, runtime = %runtime,
            work_dir = %work_dir.display(), mode = %mode, "launching acp session"
        );
        let session = session_mod::insert(&st.db, &new_session).await?;
        // A custom acp agent supplies its own adapter command; a builtin
        // resolves its adapter (claude-agent-acp / codex-acp).
        let custom = if agent::builtin_agent_type(&runtime).is_some() {
            None
        } else {
            custom_agents::get(&st.db, &runtime).await?
        };
        let launch = agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                branch_id: &branch.id,
                runtime: &runtime,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &model,
                effort: &effort,
                goal_file: goal_file.as_deref(),
                primer_file: primer_file.as_deref(),
                extra_env: &extra_env,
                mode: &mode,
                custom: custom.as_ref(),
            },
            agent::AcpOpen::Fresh,
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Err(e) = crate::acp::start(&st, &session.id, launch).await {
            // Mark the row errored (visible, inspectable) rather than leaving a
            // running row with no live task behind it.
            let _ = session_mod::set_status(&st.db, &session.id, "error").await;
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "status",
                json!({ "status": "error", "reason": "acp launch failed" }),
            )
            .await
            .ok();
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("acp launch failed: {e}"),
            ));
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
        agent::launch(
            &st.db,
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
        tracing::info!(session = %session_id, branch = %branch.id, "agent terminal launched");
        session_mod::insert(&st.db, &new_session).await?
    };
    tracing::debug!(session = %session.id, status = %status, "inserted session row");

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
        tracing::debug!(branch = %branch.id, "stamping concierge idle mark");
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
pub(super) async fn get_chat(State(st): State<AppState>) -> ApiResult<Json<SessionView>> {
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
    tracing::info!(repo = %home.repo_root, "launching fleet concierge session");
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
pub(super) async fn reset_chat(State(st): State<AppState>) -> ApiResult<Json<SessionView>> {
    tracing::info!("resetting fleet concierge chat");
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

/// The session's launch prompt: a pointer to the goal rather than a copy of
/// it. The goal lives once, as the `goal` artifact (`weaver summary` opens
/// with it) — pasting it here made a second copy that drifted the moment the
/// agent revved the artifact. The pointer routes through the `weaver` CLI so
/// it orients hookless agents (Codex, custom) too, which never receive the
/// WEAVER.md primer. Mirrors [`scratch_note`]: it rides on the prompt only
/// (goal.txt), never the stored goal.
fn entrance_note(title: &str, tracking_issue: Option<i64>) -> String {
    let mut note = format!(
        "Your task: {title}.\n\n\
         Run `weaver summary` first — it prints the full goal, your current \
         status, and the open tasks. `weaver readme` prints the complete \
         workflow guide when it is not already in your context."
    );
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
    let (session, branch) = require_session(&st.db, &key).await?;
    if let Some(title) = &req.title {
        branch_mod::set_title(&st.db, &branch.id, title).await?;
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
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
    // Park override — the fleet list's resting shelf. `"auto"` clears the manual
    // override back to idle-driven (stored NULL); `"parked"` / `"active"` pin it.
    if let Some(park) = &req.park {
        let stored = match park.as_str() {
            "auto" => None,
            "parked" => Some("parked"),
            "active" => Some("active"),
            other => return Err(AppError::bad_request(format!("invalid park '{other}'"))),
        };
        session_mod::set_park(&st.db, &session.id, stored).await?;
    }
    if let Some(order) = req.sort_order {
        session_mod::set_sort_order(&st.db, &session.id, order).await?;
    }
    let (session, branch) = require_session(&st.db, &session.id).await?;
    Ok(Json(session_view(&st.db, &session, &branch).await?))
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
    let (session, branch) = require_session(&st.db, &key).await?;
    tracing::info!(session = %session.id, branch = %branch.id, keep_branch = q.keep_branch, "deleting session");
    let mut warnings: Vec<String> = Vec::new();

    backend::kill_session(&session.term_session).await.ok();
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    tracing::debug!(session = %session.id, "killed terminal, debug shells, and ide sessions");
    if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
        warnings.push(format!("worktree remove: {e}"));
        tokio::fs::remove_dir_all(&work_dir).await.ok();
    }
    if !q.keep_branch {
        tracing::debug!(session = %session.id, branch_name = %branch.branch, "deleting git branch");
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
    if warnings.is_empty() {
        tracing::info!(session = %session.id, branch = %branch.id, keep_branch = q.keep_branch, "session deleted");
    } else {
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
pub(crate) async fn archive(
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

    // Stop the ACP task (drops its handle so the task winds down) before killing
    // the relay supervisor below — for a terminal session this is a no-op.
    if session.protocol == "acp" {
        st.acp.stop(&session.id);
    }
    backend::kill_session(&session.term_session).await.ok();
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
    let (session, branch) = require_session(&st.db, &key).await?;
    tracing::debug!(key = %key, session = %session.id, "handling archive session request");
    let warnings = archive(&st, &session, &branch).await?;
    Ok(Json(
        json!({ "archived": true, "branch": branch.branch, "warnings": warnings }),
    ))
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
    branch_mod::set_title(&st.db, &branch.id, &format!("watch {}", watch.name)).await?;
    tracing::debug!(watch = %watch.id, branch = %branch.id, "upserted warm session branch row");

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    tracing::debug!(watch = %watch.id, session = %session_id, "allocated warm session id and run dir");

    // The warm session runs the configured default agent (the watch's
    // judging agent, normally `claude`); its `prompt` param, when set, seeds the
    // first turn.
    let agent = configured_agent(&st.db, "agent.default", config::DEFAULT_AGENT).await;
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
    let extra_env = launch_env(&st.db, &repo_root, &repo_cfg).await;
    // A warm session never carries the concierge role, so its runtime is its kind.
    tracing::info!(watch = %watch.id, session = %session_id, agent = %agent, work_dir = %work_dir.display(), "launching warm session agent terminal");
    agent::launch(
        &st.db,
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &agent,
            work_dir: &work_dir,
            term_session: &term_session,
            goal_file: goal_file.as_deref(),
            // A warm watch session is never the concierge, so no primer.
            primer_file: None,
            server_addr: &st.addr,
            model: &watch.model,
            effort: &watch.effort,
            extra_env: &extra_env,
        },
        agent::LaunchMode::Fresh,
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(watch = %watch.id, session = %session_id, "warm session agent terminal launched");

    let status = agent::initial_status(&st.db, &agent).await;
    let session = session_mod::insert(
        &st.db,
        &NewSession {
            id: session_id,
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            term_session,
            agent_kind: agent,
            model: watch.model.clone(),
            effort: watch.effort.clone(),
            status: status.to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(watch.id.clone()),
            // Engine-created infrastructure, no user behind it.
            created_by: None,
            // Warm sessions stay on the terminal backend: the watch engine
            // drives the judge by typing into its PTY, a flow the acp prompt
            // queue does not replace yet.
            protocol: "terminal".to_string(),
        },
    )
    .await?;

    repo::record_use(&st.db, &repo_root_str).await.ok();
    tracing::info!(
        watch = %watch.id,
        session = %session.id,
        "warm session created"
    );
    Ok(session)
}

/// Recreate an orphaned session's terminal and resume its agent. The worktree is
/// expected to still be on disk (an orphaned session only lost its terminal); a
/// missing worktree is an error here — recovering a *torn-down* (archived)
/// session, which rebuilds the worktree first, goes through [`recover`].
pub(crate) async fn adopt(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<(), AppError> {
    if session.protocol == "acp" {
        return adopt_acp(st, session, branch).await;
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
    let runtime = launch_runtime(&st.db, &session.agent_kind).await;
    let declares_acp = matches!(
        agent::metadata_for(&st.db, &runtime).await?,
        Some(meta) if meta.builtin && meta.protocol == "acp"
    );
    if declares_acp {
        return adopt_terminal_into_acp(st, session, branch, &runtime).await;
    }
    resume_agent(st, session, branch, "session adopted").await
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
    let extra_env = launch_env(&st.db, &repo_root, &repo_cfg).await;
    let run_dir = db::run_dir(&session.id);
    let primer_file = {
        let f = run_dir.join("primer.txt");
        f.exists().then_some(f)
    };
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
            branch_id: &branch.id,
            runtime,
            work_dir: &work_dir,
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            extra_env: &extra_env,
            // Terminal rows carry no mode; the seeded-gates posture they ran
            // under matches the acp default.
            mode: agent::DEFAULT_ACP_MODE,
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
async fn adopt_acp(st: &AppState, session: &Session, branch: &Branch) -> Result<(), AppError> {
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
        let extra_env = launch_env(&st.db, &repo_root, &repo_cfg).await;
        let runtime = launch_runtime(&st.db, &session.agent_kind).await;
        let custom = if agent::builtin_agent_type(&runtime).is_some() {
            None
        } else {
            custom_agents::get(&st.db, &runtime).await?
        };
        let run_dir = db::run_dir(&session.id);
        let primer_file = {
            let f = run_dir.join("primer.txt");
            f.exists().then_some(f)
        };
        let goal_file = {
            let f = run_dir.join("goal.txt");
            f.exists().then_some(f)
        };
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
                branch_id: &branch.id,
                runtime: &runtime,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &session.model,
                effort: &session.effort,
                goal_file: goal_file.as_deref(),
                primer_file: primer_file.as_deref(),
                extra_env: &extra_env,
                mode: &mode,
                custom: custom.as_ref(),
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
        json!({ "status": status, "reason": "session adopted" }),
    )
    .await
    .ok();
    tracing::info!(session = %session.id, branch = %branch.id, "acp session adopted");
    Ok(())
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
) -> Result<(), AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, reason = %reason, "resuming agent");
    let work_dir = PathBuf::from(&session.work_dir);
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
        if f.exists() {
            // Refresh from the authoritative goal artifact before the spawned
            // shell cats this file in as the opening prompt, so a restart picks
            // up the newest goal rather than reseeding a stale on-disk copy. A
            // failure here is non-fatal — the existing goal.txt is still a valid
            // prompt — but log it so a silently-stale goal is diagnosable.
            match branch_mod::current_goal(&st.db, branch).await {
                Ok(goal) => {
                    if let Err(e) = tokio::fs::write(&f, &goal).await {
                        tracing::warn!(error = %e, "failed to refresh goal.txt on adopt");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to read goal for adopt refresh"),
            }
            tracing::debug!(session = %session.id, "refreshed goal file for resume");
            Some(f)
        } else {
            None
        }
    };
    // Re-launch with the same layered env the session started with, so a resumed
    // session keeps its per-repo / config-file environment (not just the global
    // agent_env). Setup is NOT re-run on adopt — the worktree is already
    // provisioned; this only resumes the agent.
    let repo_root = PathBuf::from(&branch.repo_root);
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let extra_env = launch_env(&st.db, &repo_root, &repo_cfg).await;
    let runtime = launch_runtime(&st.db, &session.agent_kind).await;
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
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            extra_env: &extra_env,
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
async fn recover(st: &AppState, session: &Session, branch: &Branch) -> Result<(), AppError> {
    tracing::info!(session = %session.id, branch = %branch.id, "recovering archived session");
    if backend::has_session(&session.term_session).await {
        return Err(AppError::conflict(
            "session already has a running terminal process",
        ));
    }
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);

    // Rebuild the worktree if archive removed it. Archive keeps the branch, but a
    // later manual `git branch -D` could have deleted it — refuse clearly rather
    // than let the checkout fail cryptically.
    if !work_dir.exists() {
        if !git::branch_exists(&repo_root, &branch.branch).await {
            return Err(AppError::bad_request(format!(
                "branch '{}' no longer exists — cannot recover",
                branch.branch
            )));
        }
        // Clear any stale worktree registration at this path first: archive's
        // forced remove deregisters, but a manual `rm -rf` of the dir would leave
        // git's admin entry behind and reject re-adding the same path.
        git::worktree_prune(&repo_root).await.ok();
        tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
        git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
        tracing::info!(session = %session.id, branch = %branch.id, work_dir = %work_dir.display(), "rebuilding worktree for recovered session");
        git::worktree_add_existing(&repo_root, &work_dir, &branch.branch)
            .await
            .map_err(|e| AppError::bad_request(e.to_string()))?;
    } else {
        tracing::debug!(session = %session.id, "worktree still present, skipping rebuild");
    }

    tags::set(
        &st.db,
        &branch.id,
        tags::RECOVERED_KEY,
        tags::RECOVERED_VALUE,
        "session recovered",
        "loom",
    )
    .await?;
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
    tracing::debug!(session = %session.id, branch = %branch.id, "marked session recovered, resuming agent");
    resume_agent(st, session, branch, "session recovered").await?;
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
) -> ApiResult<Json<weaver_core::transcript::Log>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    match crate::chatlog::conversation(&st.db, &session, &branch).await {
        Some(log) => Ok(Json(log)),
        None => Err(AppError::not_found("conversation")),
    }
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
    // For an ACP session a send *is* a `session/prompt` (queued when a turn is in
    // flight): delegate to the prompt queue, keeping the same `nudge` audit. This
    // is what makes `loom session send` work uniformly across both backends.
    if session.protocol == "acp" {
        let handle = require_acp_task(&st, &session)?;
        let by = author_or_manual(req.by.as_deref());
        let ack = handle
            .prompt(req.text.clone(), Some(by.clone()))
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
        return Ok(Json(
            json!({ "sent": true, "submitted": true, "queued": ack.queued, "turn": ack.turn }),
        ));
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
// person or watch uses — a `session/prompt` queueing send (`/prompt`), a
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

/// The in-flight turn number from a session's persisted `acp_inflight`, or `None`.
fn live_turn(session: &Session) -> Option<i64> {
    session
        .acp_inflight
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("turn").and_then(Value::as_i64))
}

/// The journaled conversation: `{ blocks: [...], live_turn }`. Works whether or
/// not a task is currently running (it reads the durable journal).
pub(super) async fn get_session_chat(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let blocks = crate::chat::list(&st.db, &session.id).await?;
    Ok(Json(
        json!({ "blocks": blocks, "live_turn": live_turn(&session) }),
    ))
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
}

/// Send a user message to an ACP session: dispatched as a `session/prompt` when
/// idle, appended to the durable queue when a turn is in flight. Returns 202
/// `{ queued, turn }`. Every send records a `nudge` event (the audit rule).
pub(super) async fn prompt_session(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PromptBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (session, branch) = require_session(&st.db, &key).await?;
    require_acp(&session)?;
    let handle = require_acp_task(&st, &session)?;
    let by = author_or_manual(req.by.as_deref());
    let ack = handle
        .prompt(req.text.clone(), Some(by.clone()))
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
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "queued": ack.queued, "turn": ack.turn })),
    ))
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
        ensure_github_token_available(&db, &[], Some("alice"), "claude")
            .await
            .unwrap();

        crate::user_token::remove(&db, "alice").await.unwrap();
        ensure_github_token_available(
            &db,
            &[("GH_TOKEN".to_string(), "ghp_shared".to_string())],
            Some("alice"),
            "codex",
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
        ensure_github_token_available(&db, &[], Some("alice"), "my-custom-agent")
            .await
            .unwrap();
        // A webhook launch carries an attribution string, not an approved user.
        ensure_github_token_available(&db, &[], Some("github-webhook (octo)"), "codex")
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
    fn entrance_note_points_at_the_goal_instead_of_pasting_it() {
        let note = entrance_note("Wire the flux capacitor", Some(42));
        assert!(note.contains("Wire the flux capacitor"));
        // The orientation is a pointer, not a copy: the goal is fetched.
        assert!(note.contains("weaver summary"));
        // It tells the agent exactly how to signal "done".
        assert!(note.contains("weaver issue #42"));
        assert!(note.contains("weaver issue close 42"));
        assert!(note.contains("weaver status"));
        // Untracked sessions get the orientation with no issue contract.
        let untracked = entrance_note("Poke around", None);
        assert!(untracked.contains("weaver summary"));
        assert!(!untracked.contains("issue"));
    }
}
