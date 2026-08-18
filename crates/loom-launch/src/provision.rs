//! Ordinary session provisioning with one-way dependencies on runtime and
//! domain owners, never Axum or the REST adapter.

use std::path::PathBuf;

use serde_json::json;
use weaver_api::{CreateReq, LaunchOverrides, LaunchSelection, ResolvedLaunchView};
use weaver_core::branch as branch_mod;
use weaver_core::branch::{Branch, TitleProvenance};
use weaver_core::tags;
use weaver_core::BoxFut;

use crate::auth::{Grant, Principal};
use crate::runtime::{configure_session_github_auth, layer_launch_environment, set_env};
use crate::scratch::{prepare_initial_scratch, scratch_note, write_prepared_initial_scratch};
use crate::session::{self as session_mod, NewSession, Session};
use crate::{agent, config, db, events, git, github, repo, setup, AppState, Db};

#[derive(Debug)]
pub enum ProvisionError {
    Invalid(String, Option<Box<ResolvedLaunchView>>),
    Forbidden(String),
    NotFound(String),
    Conflict(String, Option<Box<ResolvedLaunchView>>),
    CredentialRequired(String),
    ExternalFailure(String, Option<String>),
    Internal(anyhow::Error),
}

impl ProvisionError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into(), None)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into(), None)
    }

    fn not_found(what: &str) -> Self {
        Self::NotFound(format!("{what} not found"))
    }

    fn external_failure(message: impl Into<String>) -> Self {
        Self::ExternalFailure(message.into(), None)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(anyhow::anyhow!(message.into()))
    }

    fn with_preview(self, preview: ResolvedLaunchView) -> Self {
        match self {
            Self::Invalid(message, _) => Self::Invalid(message, Some(Box::new(preview))),
            Self::Conflict(message, _) => Self::Conflict(message, Some(Box::new(preview))),
            error => error,
        }
    }

    fn with_session_id(self, session_id: impl Into<String>) -> Self {
        match self {
            Self::ExternalFailure(message, _) => {
                Self::ExternalFailure(message, Some(session_id.into()))
            }
            error => error,
        }
    }
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message, _)
            | Self::Forbidden(message)
            | Self::NotFound(message)
            | Self::Conflict(message, _)
            | Self::CredentialRequired(message)
            | Self::ExternalFailure(message, _) => f.write_str(message),
            Self::Internal(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ProvisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for ProvisionError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<std::io::Error> for ProvisionError {
    fn from(error: std::io::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<crate::scratch::ScratchError> for ProvisionError {
    fn from(error: crate::scratch::ScratchError) -> Self {
        match error {
            crate::scratch::ScratchError::Invalid(message) => Self::Invalid(message, None),
            crate::scratch::ScratchError::NotFound(message) => Self::NotFound(message),
            crate::scratch::ScratchError::Internal(error) => Self::Internal(error),
        }
    }
}

type Result<T> = std::result::Result<T, ProvisionError>;

#[derive(Debug)]
pub struct Provisioned {
    pub session: Session,
    pub branch: Branch,
}

#[derive(Debug, Clone)]
enum ActorKind {
    Admin {
        username: String,
        delegated: bool,
    },
    Producer {
        origin: &'static str,
        subject: String,
    },
    Automation {
        origin: String,
        subject: String,
        profiles: Vec<String>,
        run_id: Option<String>,
        session_id: Option<String>,
    },
    Session {
        username: String,
        session_id: String,
        branch_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct Actor(ActorKind);

impl Actor {
    pub fn from_principal(principal: &Principal, delegated: bool) -> Self {
        match &principal.grant {
            Grant::Admin | Grant::User => Self(ActorKind::Admin {
                username: principal.username.clone(),
                delegated,
            }),
            Grant::Automation { subject, profiles } => Self(ActorKind::Automation {
                origin: "automation".to_string(),
                subject: subject.clone(),
                profiles: profiles.clone(),
                run_id: None,
                session_id: None,
            }),
            Grant::Session {
                session_id,
                branch_id,
                ..
            } => Self(ActorKind::Session {
                username: principal.username.clone(),
                session_id: session_id.clone(),
                branch_id: branch_id.clone(),
            }),
        }
    }

    pub fn producer(origin: &'static str, subject: impl Into<String>) -> Self {
        debug_assert!(matches!(
            origin,
            "github" | "slack" | "watch" | "monitor" | "startup"
        ));
        Self(ActorKind::Producer {
            origin,
            subject: subject.into(),
        })
    }

    pub fn automation(
        origin: impl Into<String>,
        subject: impl Into<String>,
        profiles: Vec<String>,
        run_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self(ActorKind::Automation {
            origin: origin.into(),
            subject: subject.into(),
            profiles,
            run_id: Some(run_id.into()),
            session_id: Some(session_id.into()),
        })
    }

    fn origin(&self) -> &str {
        match &self.0 {
            ActorKind::Admin {
                delegated: true, ..
            }
            | ActorKind::Session { .. } => "agent",
            ActorKind::Admin { .. } => "user",
            ActorKind::Producer { origin, .. } => origin,
            ActorKind::Automation { origin, .. } => origin,
        }
    }

    fn display_creator(&self) -> Option<String> {
        match &self.0 {
            ActorKind::Admin { username, .. } | ActorKind::Session { username, .. } => {
                Some(username.clone())
            }
            ActorKind::Producer { subject, .. } | ActorKind::Automation { subject, .. } => {
                Some(subject.clone())
            }
        }
    }

    pub fn bound_parent_branch(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Session { branch_id, .. } => Some(branch_id),
            _ => None,
        }
    }

    fn bound_parent_session(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Session { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    fn creator_identity(&self) -> (&'static str, String) {
        match &self.0 {
            ActorKind::Admin { username, .. } => ("user", username.clone()),
            ActorKind::Producer { subject, .. } => ("system", subject.clone()),
            ActorKind::Automation { subject, .. } => ("automation", subject.clone()),
            ActorKind::Session { session_id, .. } => ("session", session_id.clone()),
        }
    }

    fn allowed_profiles(&self) -> Option<&[String]> {
        match &self.0 {
            ActorKind::Automation { profiles, .. } => Some(profiles),
            _ => None,
        }
    }

    fn automation_run_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { run_id, .. } => run_id.as_deref(),
            _ => None,
        }
    }

    fn reserved_session_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }
}

async fn fetch_launch_issue(
    st: &AppState,
    repo_root: &std::path::Path,
    managed_repo: Option<&crate::repo::RepoSlug>,
    number: i64,
) -> anyhow::Result<github::Issue> {
    let slug = match managed_repo {
        Some(repo) => repo.clone(),
        None => crate::repo::github_slug_for_root(&st.db, repo_root)
            .await?
            .and_then(|slug| crate::repo::parse_slug(&slug).ok())
            .ok_or_else(|| anyhow::anyhow!("repository has no registered GitHub identity"))?,
    };
    let app = st
        .trigger
        .app()
        .ok_or_else(|| anyhow::anyhow!("GitHub App is unavailable"))?;
    app.issue(&slug.owner, &slug.name, number).await
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

fn create_selection(req: &CreateReq) -> Result<LaunchSelection> {
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
        return Err(ProvisionError::invalid(
            "canonical `selection` cannot be combined with flattened launch selectors",
        ));
    }
    if req.expected_profile_revision.is_none() || req.expected_resolver_revision.is_none() {
        return Err(ProvisionError::invalid(
            "canonical `selection` requires expected_profile_revision and expected_resolver_revision from a resolve preview",
        ));
    }
    Ok(selection.clone())
}

/// Boxed so this future's state machine is codegen'd here, in `loom-launch`,
/// rather than re-instantiated in whichever crate ends up polling it. An
/// `async fn` body is emitted where it is awaited, and every caller of this one
/// is itself an `async fn`, so the whole chain otherwise bubbles up into the
/// root `loom` crate — 62k lines of LLVM IR on every `loom` rebuild.
pub fn create(st: AppState, req: CreateReq, actor: Actor) -> BoxFut<'static, Result<Provisioned>> {
    Box::pin(create_inner(st, req, actor))
}

async fn create_inner(st: AppState, req: CreateReq, actor: Actor) -> Result<Provisioned> {
    let created_by = actor.display_creator();
    let origin = actor.origin();
    tracing::info!(
        repo = ?req.repo,
        agent = ?req.agent,
        created_by = ?created_by,
        origin,
        "starting session creation"
    );
    // Attachment input is untrusted launch input, not a provisioning step.
    // Decode and validate the entire batch before touching a repository,
    // worktree, branch, work-item claim, or session row.
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
    let resolved = match crate::launch::resolve(&st.db, &selection, &options).await {
        Ok(resolved) => resolved,
        Err(error) if req.expected_resolver_revision.is_some() => {
            return Err(ProvisionError::conflict(format!(
                "launch settings can no longer be resolved after preview: {}",
                error
            )));
        }
        Err(error) => return Err(ProvisionError::invalid(error.to_string())),
    };
    let stale_profile = req
        .expected_profile_revision
        .is_some_and(|expected| expected != resolved.view.profile_revision);
    let stale_resolver = req
        .expected_resolver_revision
        .as_deref()
        .is_some_and(|expected| expected != resolved.view.resolver_revision);
    if stale_profile || stale_resolver {
        return Err(ProvisionError::conflict(
            "launch settings changed since preview; review the fresh resolution",
        )
        .with_preview(resolved.view));
    }
    if !resolved.view.valid {
        let message = resolved
            .view
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| "launch settings are not valid".to_string());
        let error = if resolved.view.capacity.allowed {
            ProvisionError::invalid(message)
        } else {
            ProvisionError::conflict(message)
        };
        return Err(error.with_preview(resolved.view));
    }
    // Resolve write-only values once, then confirm the template revision still
    // matches. Environment mutations advance that revision transactionally, so
    // provisioning below uses this concrete snapshot instead of silently
    // reading a value changed after preview.
    let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
        .await
        .map_err(|error| ProvisionError::invalid(error.to_string()))?;
    let current_profile = crate::profile::get(&st.db, &resolved.profile.name)
        .await?
        .ok_or_else(|| ProvisionError::invalid("selected profile was removed after preview"))?;
    if current_profile.revision != resolved.view.profile_revision
        || current_profile.lifetime != resolved.view.profile_lifetime
    {
        let fresh = crate::launch::resolve(&st.db, &selection, &options)
            .await
            .map_err(|error| ProvisionError::invalid(error.to_string()))?;
        return Err(ProvisionError::conflict(
            "launch profile changed while resolving its environment; review the fresh resolution",
        )
        .with_preview(fresh.view));
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
            .map_err(|error| ProvisionError::invalid(error.to_string()))?;
    if let Some(allowed) = actor.allowed_profiles() {
        if !allowed.iter().any(|name| name == &profile_name) {
            return Err(ProvisionError::Forbidden(format!(
                "automation grant does not allow profile '{profile_name}'"
            )));
        }
        if !launch_profile.is_automation_safe() {
            return Err(ProvisionError::invalid(format!(
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
                repo::ResolveError::BadRequest(m) => ProvisionError::invalid(m),
                repo::ResolveError::Clone(m) => ProvisionError::external_failure(m),
            })?,
        None => {
            let cwd = PathBuf::from(&req.cwd);
            git::repo_root(&cwd)
                .await
                .map_err(|e| ProvisionError::invalid(e.to_string()))?
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
        let fresh = crate::launch::resolve(&st.db, &selection, &options)
            .await
            .map_err(|error| ProvisionError::invalid(error.to_string()))?;
        return Err(ProvisionError::conflict(format!(
            "profile '{profile_name}' has reached its max_concurrent limit ({})",
            launch_profile.max_concurrent
        ))
        .with_preview(fresh.view));
    }

    // Now acquire the managed clone (inside the gate), or reuse the local root
    // resolved above. The traversal / allowlist boundary lives in `repo`.
    let repo_root = match managed_repo {
        Some(input) => repo::resolve_clone(&st.db, input, st.trigger.app())
            .await
            .map_err(|e| match e {
                repo::ResolveError::BadRequest(m) => ProvisionError::invalid(m),
                repo::ResolveError::Clone(m) => ProvisionError::external_failure(m),
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
                return Err(ProvisionError::invalid(format!(
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
    // preflight and issue seeding an exact installation target; local paths
    // resolve their registered identity or origin without contacting GitHub.
    let managed_slug = req
        .repo
        .as_deref()
        .and_then(|repo| crate::repo::parse_slug(repo).ok());
    let current_github_repo = match managed_slug.as_ref() {
        Some(repo) => Some(repo.slug()),
        None => repo::github_slug_for_root(&st.db, &repo_root).await?,
    };
    let configured_github_repositories = launch_profile
        .github_repositories()
        .map_err(|error| ProvisionError::invalid(error.to_string()))?;
    let session_github_repositories = crate::runtime::session_github_repositories(
        &class,
        &configured_github_repositories,
        current_github_repo.as_deref(),
    );
    let stamped_github_repositories = serde_json::to_string(&session_github_repositories)
        .map_err(|error| ProvisionError::invalid(error.to_string()))?;
    let runtime = agent.clone();
    tracing::debug!(agent = %agent, runtime = %runtime, "resolved agent runtime");
    // The resolved launch environment: selected profile < per-repo repo_env <
    // the repo file's [env]. It is needed before provisioning so a real agent
    // launch can stop cleanly when the selected profile has no App access.
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
            .map_err(|e| ProvisionError::invalid(e.to_string()))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }
    tracing::debug!(model = %model, effort = %effort, protocol = %protocol, "resolved and validated model/effort/protocol");
    let github_app = if (!session_github_repositories.is_empty())
        || (launch_profile.restricted && managed_slug.is_some())
    {
        st.trigger.app()
    } else {
        None
    };
    if current_github_repo.is_some()
        && !crate::runtime::github_credential_available(
            &st.db,
            created_by.as_deref(),
            github_app,
            crate::runtime::user_github_token_allowed(&class, launch_profile.restricted),
        )
        .await?
    {
        return Err(ProvisionError::CredentialRequired(
            crate::runtime::MISSING_GITHUB_TOKEN_MESSAGE.to_string(),
        ));
    }
    tracing::debug!(runtime = %runtime, "github app availability check passed");

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
            .map_err(|e| ProvisionError::invalid(format!("issue #{number}: {e}")))?;
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
            None => crate::repo::github_slug_for_root(&st.db, &repo_root).await?,
        };
        tracing::debug!(issue = number, github_repo = ?github_repo, "seeded session fields from github issue");
    } else if let Some(number) = req.github_issue {
        // The caller already holds the thread (the `@loom` trigger): record the
        // GitHub link on the compatibility work item without fetch-and-seed.
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
            .ok_or_else(|| ProvisionError::not_found("issue"))?;
        if issue.repo_root != repo_root_str {
            return Err(ProvisionError::invalid(format!(
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
        return Err(ProvisionError::invalid(
            "`name` and `existing_branch` are mutually exclusive",
        ));
    }

    // Unless the caller pins a base, fork from a freshly-fetched `origin/<default
    // branch>` so new work starts from the latest mainline, not the launching
    // checkout's (possibly stale) current branch. `default_base` degrades to the
    // current branch on a remote-less repo.
    let mut base = match req.base.clone() {
        Some(b) => b,
        None => git::default_base(&repo_root).await?,
    };
    tracing::debug!(base = %base, "resolved base branch");

    let (branch_name, work_dir) = if let Some(existing_branch) = existing {
        tracing::info!(branch = %existing_branch, "reusing existing branch for session");
        if !git::branch_exists(&repo_root, existing_branch).await {
            return Err(ProvisionError::invalid(format!(
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
                return Err(ProvisionError::conflict(format!(
                    "branch '{existing_branch}' already has an active session"
                )));
            }
        }
        let work_dir = match git::worktree_for_branch(&repo_root, existing_branch)
            .await
            .map_err(|e| ProvisionError::invalid(e.to_string()))?
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
                    .map_err(|e| ProvisionError::invalid(e.to_string()))?;
                dir
            }
        };
        (existing_branch.to_string(), work_dir)
    } else {
        // Create `weaver/<slug>` with a unique suffix. Resolve the base against
        // origin too — fetching on demand — so a branch that exists on the remote
        // but not yet in this checkout is a valid fork point; the recorded base
        // then names the ref actually forked from (e.g. `origin/<name>`).
        match git::resolve_base(&repo_root, &base).await {
            Some(resolved) => base = resolved,
            None => {
                return Err(ProvisionError::invalid(git::missing_revision_message(
                    &repo_root, &base,
                )));
            }
        }
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
                return Err(ProvisionError::conflict(format!(
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
            .map_err(|e| ProvisionError::invalid(e.to_string()))?;
        (branch_name, work_dir)
    };

    // A replacement launch may reuse a worktree whose previous session left
    // Scratch files behind. Validate and write the merged set while holding the
    // same path-scoped permit as live upload/delete routes, before creating any
    // branch row, work-item claim, or session.
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
        .ok_or_else(|| ProvisionError::internal("branch vanished"))?;
    tracing::debug!(branch = %branch.id, title = %title, "stamped branch title/goal/description");

    // Resolve the repository-scoped launching branch once: it names an
    // explicitly claimed work item's `source_branch` and supplies the legacy
    // `parent_branch_id` fallback. Only attribute that branch link inside this
    // repo, and never to the branch itself — `resolve_key` searches globally,
    // so a stray `$WEAVER_BRANCH` from a checkout elsewhere must not
    // misattribute work-item provenance to an unrelated repository.
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
    // Session ancestry is global, unlike branch/work-item provenance. A scoped
    // session credential identifies the exact launcher even when the child is
    // created in another repository. Admin compatibility launches have no
    // bound session, so retain the same-repo active-branch lookup for them.
    let parent_session_id = if let Some(parent_session_id) = actor.bound_parent_session() {
        Some(parent_session_id.to_string())
    } else {
        match &parent {
            Some(parent) => session_mod::active_for_branch(&st.db, &parent.id)
                .await?
                .map(|session| session.id),
            None => None,
        }
    };
    let stamped_allowed_tools = serde_json::to_string(&resolved.runtime_permissions)
        .map_err(|error| ProvisionError::invalid(error.to_string()))?;
    let stamped_mcp_access = serde_json::to_string(&resolved.mcp_policy)
        .map_err(|error| ProvisionError::invalid(error.to_string()))?;
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
        github_repositories: stamped_github_repositories,
        allowed_tools: stamped_allowed_tools.clone(),
        mcp_access: stamped_mcp_access,
        launch_snapshot,
        creator_kind: creator_kind.to_string(),
        creator_subject,
        parent_session_id,
        automation_run_id: actor.automation_run_id().map(str::to_string),
    };

    // Keep an explicit claimed/imported work item attached for compatibility.
    // Ordinary and delegated launches use their default channel instead of
    // manufacturing a second task object.
    tracing::debug!(branch = %branch.id, "resolving explicit work item for session");
    let tracking_issue = resolve_explicit_work_item(
        &st,
        &branch,
        parent_branch_name.as_deref(),
        &title,
        &description,
        github_repo.as_deref(),
        github_issue,
        claimed_issue_id,
    )
    .await?;
    tracing::debug!(branch = %branch.id, tracking_issue = ?tracking_issue, "explicit work item resolved");

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
            &launch_profile.instructions,
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
                return Ok(Provisioned { session, branch });
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
    let session_token = crate::auth::create_session_token_with_policy(
        &st.db,
        created_by.as_deref(),
        &session_id,
        &branch.id,
        launch_profile.restricted,
        &launch_policy.mcp_access,
    )
    .await?;
    configure_session_github_auth(
        &st.db,
        &mut extra_env,
        created_by.as_deref(),
        &class,
        launch_profile.restricted,
        github_app,
    )
    .await;
    set_env(&mut extra_env, "LOOM_TOKEN", session_token);
    set_env(&mut extra_env, "LOOM_SESSION_ID", session_id.clone());
    let session = if protocol == "acp" {
        // The ACP path inserts the row *first* — `acp::start` binds a relay to it
        // and reads it back — then brings up the headless adapter over the relay.
        tracing::info!(
            session = %session_id, branch = %branch.id, runtime = %runtime,
            work_dir = %work_dir.display(), mode = %mode, "launching acp session"
        );
        let session =
            crate::session_layout::insert_session(&st.db, &st.bus, &new_session, &launch_policy)
                .await?;
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
        .map_err(|e| ProvisionError::internal(e.to_string()))?;
        if let Err(e) = crate::acp::start(&st.acp_ctx(), &session.id, launch).await {
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
                ProvisionError::external_failure(format!("acp launch failed: {e}"))
                    .with_session_id(session.id),
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
        let session =
            crate::session_layout::insert_session(&st.db, &st.bus, &new_session, &launch_policy)
                .await?;
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
            return Err(ProvisionError::internal(e.to_string()));
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
        st.acp.clone(),
        session.clone(),
        branch.clone(),
        false,
    )
    .await
    .ok();
    Ok(Provisioned { session, branch })
}

/// Session-specific operating context appended after the goal. Keep this
/// compact: the goal is the outcome, the primer owns durable workflow rules,
/// and this note only supplies the tracking contract that neither can know
/// ahead of time. `loom summary` is a recovery path, not a mandatory first
/// tool call that would inject the goal a second time.
fn entrance_note(tracking_issue: Option<i64>) -> String {
    let mut note = "You are working in a Loom session. Use `loom summary` \
                    to recover context, `loom help` to explore the registered \
                    command surface, and `loom permissions show` to inspect \
                    effective access. This session has a durable channel for \
                    user/agent messages and status history; append a typed \
                    `result` there when delegated work is complete."
        .to_string();
    if let Some(id) = tracking_issue {
        note.push_str(&format!(
            " This session is tracked as Loom issue #{id}: keep `loom \
             status set --tag <level> --message \"<message>\"` honest as you work, and run `loom \
             issues close {id}` once the task is complete (e.g. the PR is open) \
             so whoever launched you knows you are done."
        ));
    }
    note
}

/// Construct the positional first prompt from the stamped prelude policy.
/// The user's goal is always the opening user message: making an agent fetch it
/// through `loom summary` on turn one adds latency and duplicates the goal in
/// context. `none` deliberately omits all Weaver orientation.
fn build_launch_prompt(
    goal: &str,
    prelude: &str,
    instructions: &str,
    entrance: &str,
    scratch: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if !goal.is_empty() {
        parts.push(goal);
        if prelude == "weaver" {
            parts.push(entrance);
        }
    }
    let profile_instructions = profile_instructions_section(instructions);
    if let Some(instructions) = profile_instructions.as_deref() {
        parts.push(instructions);
    }
    if let Some(scratch) = scratch {
        parts.push(scratch);
    }
    parts.join("\n\n")
}

pub(crate) fn profile_instructions_section(instructions: &str) -> Option<String> {
    let instructions = instructions.trim();
    (!instructions.is_empty()).then(|| format!("## Profile instructions\n\n{instructions}"))
}

/// Adopt the explicit work item attached to a launch, if any.
///
/// `--claim <id>` and a GitHub-triggered launch keep the legacy issue
/// association because external work needs a stable mapping. Plain session
/// goals and delegated tasks live in their session channel and return `None`.
/// `source_branch` retains provenance for the explicit compatibility cases.
#[allow(clippy::too_many_arguments)]
async fn resolve_explicit_work_item(
    st: &AppState,
    branch: &Branch,
    parent_branch: Option<&str>,
    title: &str,
    description: &str,
    github_repo: Option<&str>,
    github_issue: Option<i64>,
    claim_issue: Option<i64>,
) -> Result<Option<i64>> {
    let source = parent_branch.unwrap_or(&branch.branch).to_string();
    tracing::debug!(branch = %branch.id, source = %source, "resolving explicit work item for session");

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

    Ok(None)
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
    async fn explicit_work_items_keep_provenance_without_plain_launch_issues() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = AppState {
            ctx: crate::Ctx {
                db: db.clone(),
                bus: crate::events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db.clone()),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        };
        let child = branch_mod::upsert(&db, "/r", "weaver/child", "main")
            .await
            .unwrap();

        // Ordinary delegated work lives in the child's channel, not a shadow
        // issue whose lifecycle can drift from the session.
        let plain = resolve_explicit_work_item(
            &st,
            &child,
            Some("weaver/parent"),
            "do it",
            "",
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(plain.is_none());

        // A plain top-level goal uses the same no-issue rule.
        let plain_top_level =
            resolve_explicit_work_item(&st, &child, None, "solo", "", None, None, None)
                .await
                .unwrap();
        assert!(plain_top_level.is_none());

        // Imported GitHub work remains an explicit compatibility work item and
        // retains delegation provenance.
        let imported = resolve_explicit_work_item(
            &st,
            &child,
            Some("weaver/parent"),
            "external task",
            "from GitHub",
            Some("octo/repo"),
            Some(19),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let imported = weaver_core::issue::get(&db, imported)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(imported.source_branch.as_deref(), Some("weaver/parent"));
        assert_eq!(imported.github_issue, Some(19));

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
        let claimed =
            resolve_explicit_work_item(&st, &child, None, "x", "", None, None, Some(existing.id))
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
        assert!(note.contains("loom summary"));
        assert!(note.contains("recover context"));
        assert!(!note.contains("summary` first"));
        assert!(!note.contains("prints the full goal"));
        // It tells the agent exactly how to signal "done".
        assert!(note.contains("Loom issue #42"));
        assert!(note.contains("loom issues close 42"));
        assert!(note.contains("loom status"));
        // Untracked sessions get the orientation with no issue contract.
        let untracked = entrance_note(None);
        assert!(untracked.contains("loom summary"));
        assert!(!untracked.contains("issue"));
    }

    #[test]
    fn restricted_prelude_delivers_the_caller_goal_without_weaver_orientation() {
        let goal = "Rewrite only the issue body.\nBody hash: abc123";
        let entrance = "weaver session metadata";
        assert_eq!(build_launch_prompt(goal, "none", "", entrance, None), goal);
        assert_eq!(
            build_launch_prompt(goal, "weaver", "", entrance, None),
            format!("{goal}\n\n{entrance}")
        );
    }

    #[test]
    fn profile_instructions_apply_with_or_without_the_weaver_prelude() {
        let expected = "do the work\n\n## Profile instructions\n\nUse the organization workflow.";
        assert_eq!(
            build_launch_prompt(
                "do the work",
                "none",
                "Use the organization workflow.",
                "unused",
                None,
            ),
            expected
        );
        assert_eq!(
            build_launch_prompt(
                "do the work",
                "weaver",
                "Use the organization workflow.",
                "Weaver context.",
                None,
            ),
            format!("do the work\n\nWeaver context.\n\n## Profile instructions\n\nUse the organization workflow.")
        );
    }
}
