//! Actor-aware boundary for session runtime operations.
//!
//! HTTP, GitHub, Slack, watches, and automation runs construct an [`Actor`]
//! through the narrow constructors here. Session orchestration consumes the
//! actor's derived attribution instead of accepting client-authored origin or
//! creator strings.

use crate::auth::{Grant, Principal};
use crate::web::ApiResult;
use crate::{AppState, Db};
use weaver_api::{CreateReq, SessionView};

pub(crate) const MISSING_GITHUB_TOKEN_MESSAGE: &str = "No GitHub token configured. Add your personal GitHub token in Settings > Account, or configure a write-only GH_TOKEN on the selected profile.";

/// External lifecycle work (terminal supervisors + git worktrees) cannot share
/// a SQLite transaction. Serialize those operations process-wide, then use
/// compare-and-set database transitions at their commit boundaries. The app
/// manages only hundreds of sessions, so a coarse lock keeps the invariant
/// legible without adding a per-session lock registry.
pub(crate) static LIFECYCLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn config_env_pairs(cfg: &weaver_core::repo_config::RepoConfig) -> Vec<(String, String)> {
    // Invalid shell identifiers and Loom's reserved prefixes could corrupt the
    // export or shadow the environment inherited by every agent process.
    cfg.env
        .iter()
        .filter(|(name, _)| match crate::agent_env::validate_name(name) {
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

/// Load a repo's `.weaver/config.toml`, logging and degrading to the empty
/// config on a parse error.
pub(crate) fn repo_cfg_or_default(
    repo_root: &std::path::Path,
) -> weaver_core::repo_config::RepoConfig {
    weaver_core::repo_config::load(repo_root).unwrap_or_else(|error| {
        tracing::warn!(repo = %repo_root.display(), %error,
            "ignoring malformed .weaver/config.toml");
        weaver_core::repo_config::RepoConfig::default()
    })
}

pub(crate) async fn layer_launch_environment(
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
    let repo_pairs = crate::repo_env::pairs(db, &repo_root_str)
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
        crate::repo_env::layer(&mut env, repo_pairs);
        crate::repo_env::layer(&mut env, config_pairs);
    }
    tracing::debug!(repo = %repo_root_str, profile = profile_name, strict, env_vars = env.len(), "layered launch environment");
    env
}

pub(crate) async fn apply_user_github_token(
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
        Err(error) => tracing::warn!(%username, "failed to load user github token: {error}"),
    }
}

pub(crate) fn set_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(key, _)| key == name) {
        slot.1 = value;
    } else {
        env.push((name.to_string(), value));
    }
}

fn env_has_key(env: &[(String, String)], name: &str) -> bool {
    env.iter().any(|(key, _)| key == name)
}

fn env_has_nonempty(env: &[(String, String)], name: &str) -> bool {
    env.iter()
        .any(|(key, value)| key == name && !value.trim().is_empty())
}

fn ambient_env_has_nonempty(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub(crate) async fn github_token_available(
    db: &Db,
    env: &[(String, String)],
    created_by: Option<&str>,
    runtime: &str,
    restricted_github_app: Option<&crate::github_app::GithubApp>,
) -> anyhow::Result<bool> {
    if crate::agent::builtin_agent_type(runtime).is_none() {
        return Ok(true);
    }
    let Some(username) = created_by else {
        return Ok(true);
    };
    if crate::auth::get_user(db, username).await?.is_none() {
        return Ok(true);
    }
    if env_has_nonempty(env, "GH_TOKEN")
        || (!env_has_key(env, "GH_TOKEN") && ambient_env_has_nonempty("GH_TOKEN"))
    {
        return Ok(true);
    }
    if crate::user_token::get(db, username)
        .await?
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Ok(true);
    }
    if let Some(app) = restricted_github_app {
        if app.is_configured().await {
            return Ok(true);
        }
    }
    tracing::warn!(created_by = ?created_by, runtime = %runtime, "launch blocked: no github token available");
    Ok(false)
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
pub(crate) struct Actor(ActorKind);

impl Actor {
    pub(crate) fn from_principal(principal: &Principal, delegated: bool) -> Self {
        match &principal.grant {
            Grant::Admin => Self(ActorKind::Admin {
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
            } => Self(ActorKind::Session {
                username: principal.username.clone(),
                session_id: session_id.clone(),
                branch_id: branch_id.clone(),
            }),
        }
    }

    pub(crate) fn producer(origin: &'static str, subject: impl Into<String>) -> Self {
        debug_assert!(matches!(
            origin,
            "github" | "slack" | "watch" | "monitor" | "startup"
        ));
        Self(ActorKind::Producer {
            origin,
            subject: subject.into(),
        })
    }

    pub(crate) fn automation(
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

    pub(crate) fn origin(&self) -> &str {
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

    pub(crate) fn display_creator(&self) -> Option<String> {
        match &self.0 {
            ActorKind::Admin { username, .. } | ActorKind::Session { username, .. } => {
                Some(username.clone())
            }
            ActorKind::Producer { subject, .. } | ActorKind::Automation { subject, .. } => {
                Some(subject.clone())
            }
        }
    }

    pub(crate) fn bound_parent_branch(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Session { branch_id, .. } => Some(branch_id),
            _ => None,
        }
    }

    pub(crate) fn creator_identity(&self) -> (&'static str, String) {
        match &self.0 {
            ActorKind::Admin { username, .. } => ("user", username.clone()),
            ActorKind::Producer { subject, .. } => ("system", subject.clone()),
            ActorKind::Automation { subject, .. } => ("automation", subject.clone()),
            ActorKind::Session { session_id, .. } => ("session", session_id.clone()),
        }
    }

    pub(crate) fn allowed_profiles(&self) -> Option<&[String]> {
        match &self.0 {
            ActorKind::Automation { profiles, .. } => Some(profiles),
            _ => None,
        }
    }

    pub(crate) fn automation_run_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { run_id, .. } => run_id.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn reserved_session_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }
}

/// The single actor-taking entrypoint for all session producers. HTTP, Slack,
/// GitHub, and automation runs cannot bypass attribution/grant derivation by
/// calling the web provisioning implementation directly.
pub(crate) async fn create_session(
    state: AppState,
    request: CreateReq,
    actor: Actor,
) -> ApiResult<SessionView> {
    crate::web::sessions::provision_session(state, request, actor).await
}
