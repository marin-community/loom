//! Shared session runtime policy and environment helpers.

use crate::Db;

pub const MISSING_GITHUB_TOKEN_MESSAGE: &str = "No GitHub credential configured. Add your personal GitHub token in Settings > Access, allowlist this repository for the selected profile's GitHub App credential, or configure a write-only GH_TOKEN on the profile.";
pub const GITHUB_AUTH_MODE_ENV: &str = "LOOM_GITHUB_AUTH_MODE";

/// How the image's Git/GitHub CLI adapters obtain a credential for one session.
///
/// This is stamped by Loom after it resolves the session environment. The
/// adapters must not infer policy from an inherited daemon-level `GH_TOKEN`:
/// that token belongs to the deployment, not automatically to every session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubAuthMode {
    Direct,
    Broker,
    Disabled,
}

impl GithubAuthMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Broker => "broker",
            Self::Disabled => "disabled",
        }
    }
}

/// External lifecycle work (terminal supervisors + git worktrees) cannot share
/// a SQLite transaction. Serialize those operations process-wide, then use
/// compare-and-set database transitions at their commit boundaries. The app
/// manages only hundreds of sessions, so a coarse lock keeps the invariant
/// legible without adding a per-session lock registry.
pub static LIFECYCLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
pub fn repo_cfg_or_default(repo_root: &std::path::Path) -> weaver_core::repo_config::RepoConfig {
    weaver_core::repo_config::load(repo_root).unwrap_or_else(|error| {
        tracing::warn!(repo = %repo_root.display(), %error,
            "ignoring malformed .weaver/config.toml");
        weaver_core::repo_config::RepoConfig::default()
    })
}

pub async fn layer_launch_environment(
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

pub async fn launch_environment(
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

pub async fn apply_user_github_token(
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

pub fn set_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(key, _)| key == name) {
        slot.1 = value;
    } else {
        env.push((name.to_string(), value));
    }
}

/// Stamp the GitHub credential policy consumed by the Docker image's `git`
/// credential helper and `gh` wrapper.
///
/// A resolved session token (personal, profile, repository, or config-file)
/// wins. Otherwise an allowed and configured GitHub App is brokered through the
/// session-scoped Loom API. A session with neither is disabled; the daemon's
/// ambient credential is never promoted into a managed session.
pub async fn stamp_github_auth_mode(
    env: &mut Vec<(String, String)>,
    github_app: Option<&crate::github_app::GithubApp>,
    restricted: bool,
) -> GithubAuthMode {
    let mode = if restricted {
        GithubAuthMode::Disabled
    } else if env_has_nonempty(env, "GH_TOKEN") {
        GithubAuthMode::Direct
    } else if let Some(app) = github_app {
        if app.is_configured().await {
            GithubAuthMode::Broker
        } else {
            GithubAuthMode::Disabled
        }
    } else {
        GithubAuthMode::Disabled
    };
    set_env(env, GITHUB_AUTH_MODE_ENV, mode.as_str().to_string());
    mode
}

/// Resolve a profile's App-token allowlist into the repositories stamped on
/// one session. Automation profiles intentionally retain their complete list
/// for cross-repository work. Interactive sessions receive only their current
/// repository, and only when the profile explicitly allowlists it.
pub fn session_github_repositories(
    class: &str,
    configured: &[String],
    current_repo: Option<&str>,
) -> Vec<String> {
    if class != "interactive" {
        return configured.to_vec();
    }
    current_repo
        .filter(|repo| configured.iter().any(|candidate| candidate == repo))
        .map(|repo| vec![repo.to_string()])
        .unwrap_or_default()
}

fn env_has_nonempty(env: &[(String, String)], name: &str) -> bool {
    env.iter()
        .any(|(key, value)| key == name && !value.trim().is_empty())
}

pub async fn github_token_available(
    db: &Db,
    env: &[(String, String)],
    created_by: Option<&str>,
    runtime: &str,
    github_app: Option<&crate::github_app::GithubApp>,
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
    if env_has_nonempty(env, "GH_TOKEN") {
        return Ok(true);
    }
    if crate::user_token::get(db, username)
        .await?
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Ok(true);
    }
    if let Some(app) = github_app {
        if app.is_configured().await {
            return Ok(true);
        }
    }
    tracing::warn!(created_by = ?created_by, runtime = %runtime, "launch blocked: no github token available");
    Ok(false)
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
    async fn launch_environment_respects_restricted_and_strict_profiles() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::profile::env_set(&db, "github_comment", "GH_TOKEN", "profile-token")
            .await
            .unwrap();
        crate::repo_env::set(&db, &repo.display().to_string(), "REPO_SECRET", "leak")
            .await
            .unwrap();
        let mut restricted_cfg = weaver_core::repo_config::RepoConfig::default();
        restricted_cfg
            .env
            .insert("COMMITTED_SECRET".to_string(), "leak".to_string());
        assert_eq!(
            launch_environment(&db, repo, &restricted_cfg, "github_comment", true, true).await,
            vec![("GH_TOKEN".to_string(), "profile-token".to_string())]
        );

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
        let mut strict_cfg = weaver_core::repo_config::RepoConfig::default();
        strict_cfg
            .env
            .insert("SHARED_TOKEN".to_string(), "committed".to_string());
        let strict = launch_environment(&db, repo, &strict_cfg, "default", true, false).await;
        assert_eq!(
            strict
                .iter()
                .find(|(name, _)| name == "SHARED_TOKEN")
                .map(|(_, value)| value.as_str()),
            Some("profile")
        );
        assert!(!strict.iter().any(|(name, _)| name == "CARGO_TARGET_DIR"));
    }

    #[tokio::test]
    async fn user_github_token_overlay_preserves_precedence() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        seed_user(&db, "bob").await;
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();

        let mut fresh = vec![("FOO".to_string(), "bar".to_string())];
        apply_user_github_token(&db, &mut fresh, Some("alice")).await;
        assert!(fresh
            .iter()
            .any(|(name, value)| name == "GH_TOKEN" && value == "ghp_alice"));

        let mut layered = vec![("GH_TOKEN".to_string(), "shared".to_string())];
        apply_user_github_token(&db, &mut layered, Some("alice")).await;
        let github_tokens = layered
            .iter()
            .filter(|(name, _)| name == "GH_TOKEN")
            .collect::<Vec<_>>();
        assert_eq!(github_tokens.len(), 1);
        assert_eq!(github_tokens[0].1, "ghp_alice");

        let mut fallback = vec![("GH_TOKEN".to_string(), "shared".to_string())];
        apply_user_github_token(&db, &mut fallback, Some("bob")).await;
        assert_eq!(fallback[0].1, "shared");
        let mut producer = Vec::new();
        apply_user_github_token(&db, &mut producer, None).await;
        assert!(producer.is_empty());
    }

    #[tokio::test]
    async fn github_auth_mode_is_owned_by_the_resolved_session_policy() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = crate::github_app::GithubApp::new(db.clone());

        let mut direct = vec![("GH_TOKEN".to_string(), "personal".to_string())];
        assert_eq!(
            stamp_github_auth_mode(&mut direct, Some(&app), false).await,
            GithubAuthMode::Direct
        );
        assert!(direct
            .iter()
            .any(|(name, value)| name == GITHUB_AUTH_MODE_ENV
                && value == GithubAuthMode::Direct.as_str()));

        let mut unmanaged = Vec::new();
        assert_eq!(
            stamp_github_auth_mode(&mut unmanaged, Some(&app), false).await,
            GithubAuthMode::Disabled
        );

        weaver_core::config::apply(
            &db,
            &[
                (
                    crate::github_app::APP_ID_KEY.to_string(),
                    Some("123456".to_string()),
                ),
                (
                    crate::github_app::APP_PRIVATE_KEY_KEY.to_string(),
                    Some("configured-for-mode-selection".to_string()),
                ),
            ],
        )
        .await
        .unwrap();
        let mut brokered = Vec::new();
        assert_eq!(
            stamp_github_auth_mode(&mut brokered, Some(&app), false).await,
            GithubAuthMode::Broker
        );

        let mut restricted = vec![("GH_TOKEN".to_string(), "must-not-win".to_string())];
        assert_eq!(
            stamp_github_auth_mode(&mut restricted, Some(&app), true).await,
            GithubAuthMode::Disabled
        );
    }

    #[test]
    fn interactive_github_credentials_are_scoped_to_the_current_repository() {
        let configured = vec![
            "Open-Athena/marinmirror".to_string(),
            "marin-community/marin".to_string(),
        ];
        assert_eq!(
            session_github_repositories("interactive", &configured, Some("marin-community/marin")),
            ["marin-community/marin"]
        );
        assert!(session_github_repositories(
            "interactive",
            &configured,
            Some("marin-community/loom")
        )
        .is_empty());
        assert_eq!(
            session_github_repositories("automation", &configured, None),
            configured
        );
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn github_token_preflight_covers_builtin_custom_and_app_paths() {
        let _ambient = EnvVarGuard::set("GH_TOKEN", "ghp_daemon_must_not_leak");
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        assert!(
            !github_token_available(&db, &[], Some("alice"), "codex", None)
                .await
                .unwrap()
        );
        assert!(
            github_token_available(&db, &[], Some("alice"), "my-custom-agent", None)
                .await
                .unwrap()
        );
        assert!(
            github_token_available(&db, &[], Some("github-webhook (octo)"), "codex", None)
                .await
                .unwrap()
        );

        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();
        assert!(
            github_token_available(&db, &[], Some("alice"), "claude", None)
                .await
                .unwrap()
        );
        crate::user_token::remove(&db, "alice").await.unwrap();
        assert!(github_token_available(
            &db,
            &[("GH_TOKEN".to_string(), "ghp_shared".to_string())],
            Some("alice"),
            "codex",
            None,
        )
        .await
        .unwrap());

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
        assert!(
            github_token_available(&db, &[], Some("alice"), "claude", Some(&app))
                .await
                .unwrap()
        );

        assert!(
            !github_token_available(&db, &[], Some("alice"), "codex", None,)
                .await
                .unwrap()
        );
    }
}
