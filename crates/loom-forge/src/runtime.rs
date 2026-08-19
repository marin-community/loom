//! Shared session runtime policy and environment helpers.

use crate::Db;

pub const MISSING_GITHUB_TOKEN_MESSAGE: &str = "No GitHub credential configured for this repository. Add your personal GitHub token in Settings > Account or allowlist this repository for the selected profile's GitHub App credential.";
pub const GITHUB_AUTH_MODE_ENV: &str = "LOOM_GITHUB_AUTH_MODE";

/// How the image's Git/GitHub CLI adapters obtain a credential for one session.
///
/// This is stamped by Loom after it resolves the session environment. The
/// adapters must not infer policy from process or profile environment. A
/// direct token is always the launching user's Loom-stored Account token.
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
    // Clear the stock clients' reserved credential slots before Loom selects
    // the session's Account or App identity.
    env.retain(|(name, _)| !crate::agent_env::is_github_token_name(name));
    let repo_root_str = repo_root.display().to_string();
    if restricted {
        tracing::debug!(repo = %repo_root_str, profile = profile_name, "restricted launch uses profile environment only");
        return env;
    }
    let repo_pairs = crate::repo_env::pairs(db, &repo_root_str)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !crate::agent_env::is_github_token_name(name));
    let config_pairs = config_env_pairs(cfg);
    if strict {
        // A strict profile's declared names are policy, not defaults. Repo
        // layers may add variables but cannot replace a profile-owned value.
        for (name, value) in repo_pairs.chain(config_pairs) {
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

/// Load the launching user's Loom-stored PAT into the image adapter's direct
/// credential slot after all configurable environment layers are resolved.
pub async fn apply_user_github_token(
    db: &Db,
    env: &mut Vec<(String, String)>,
    created_by: Option<&str>,
) -> bool {
    let Some(username) = created_by else {
        return false;
    };
    match crate::user_token::get(db, username).await {
        Ok(Some(token)) if !token.trim().is_empty() => {
            set_env(env, "GH_TOKEN", token);
            tracing::info!(%username, "applied Loom-stored user GitHub credential");
            true
        }
        Ok(_) => {
            tracing::debug!(%username, "no personal GitHub token on file");
            false
        }
        Err(error) => {
            tracing::warn!(%username, "failed to load user GitHub token: {error}");
            false
        }
    }
}

pub fn set_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(key, _)| key == name) {
        slot.1 = value;
    } else {
        env.push((name.to_string(), value));
    }
}

/// Personal Account PATs are available to ordinary interactive sessions.
pub fn user_github_token_allowed(class: &str, restricted: bool) -> bool {
    class == "interactive" && !restricted
}

/// Stamp the GitHub credential policy consumed by the Docker image's `git`
/// credential helper and `gh` wrapper.
///
/// A Loom-injected per-user token selects `direct`; an allowed and configured
/// GitHub App selects `broker`; restricted sessions select `disabled`. Client
/// credential slots are normalized to match the selected mode.
pub async fn stamp_github_auth_mode(
    env: &mut Vec<(String, String)>,
    github_app: Option<&crate::github_app::GithubApp>,
    restricted: bool,
    user_token_applied: bool,
) -> GithubAuthMode {
    let mode = if restricted {
        GithubAuthMode::Disabled
    } else if user_token_applied {
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
    if mode != GithubAuthMode::Direct {
        set_env(env, "GH_TOKEN", String::new());
    }
    set_env(env, "GITHUB_TOKEN", String::new());
    set_env(env, GITHUB_AUTH_MODE_ENV, mode.as_str().to_string());
    mode
}

/// Apply the launching user's Account PAT when the session class permits it,
/// then stamp the adapter mode from that result and the approved App access.
pub async fn configure_session_github_auth(
    db: &Db,
    env: &mut Vec<(String, String)>,
    created_by: Option<&str>,
    class: &str,
    restricted: bool,
    github_app: Option<&crate::github_app::GithubApp>,
) -> GithubAuthMode {
    let user_token_applied = if user_github_token_allowed(class, restricted) {
        apply_user_github_token(db, env, created_by).await
    } else {
        false
    };
    stamp_github_auth_mode(env, github_app, restricted, user_token_applied).await
}

/// The owner an `owner/*` allowlist entry covers, or `None` for a concrete
/// `owner/name` entry. A pattern scopes no token on its own: it declares which
/// repositories a session may expand into without a human decision, so a
/// brokered token stays narrow to what was actually asked for.
pub fn pattern_owner(entry: &str) -> Option<&str> {
    entry
        .split_once('/')
        .filter(|(owner, name)| *name == "*" && !owner.is_empty())
        .map(|(owner, _)| owner)
}

pub fn is_repository_pattern(entry: &str) -> bool {
    pattern_owner(entry).is_some()
}

/// The concrete `owner/name` entries of an allowlist — the only ones that can
/// scope a GitHub App installation token.
pub fn concrete_repositories(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| !is_repository_pattern(entry))
        .cloned()
        .collect()
}

/// The `owner/*` entries of an allowlist.
pub fn repository_patterns(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| is_repository_pattern(entry))
        .cloned()
        .collect()
}

/// Whether an allowlist's patterns already cover `repository`. GitHub owners
/// are case-insensitive and these patterns are hand-authored, so the owner
/// comparison is too.
pub fn pattern_allows(entries: &[String], repository: &str) -> bool {
    let Some((owner, _)) = repository.split_once('/') else {
        return false;
    };
    entries
        .iter()
        .filter_map(|entry| pattern_owner(entry))
        .any(|candidate| candidate.eq_ignore_ascii_case(owner))
}

/// Whether a stamped allowlist can scope a GitHub App token at all. Patterns
/// authorize expansion but name no repository, so an allowlist holding only
/// patterns selects no App credential.
pub fn scopes_an_app_token(entries: &[String]) -> bool {
    entries.iter().any(|entry| !is_repository_pattern(entry))
}

/// Select the App credential for a session, given the allowlist stamped on it.
/// An allowlist that names no repository scopes no token, so it selects no App
/// however the deployment is configured.
pub fn app_for_allowlist<'a>(
    entries: &[String],
    app: Option<&'a crate::github_app::GithubApp>,
) -> Option<&'a crate::github_app::GithubApp> {
    scopes_an_app_token(entries).then_some(app).flatten()
}

/// Resolve a profile's App-token allowlist into the repositories stamped on
/// one session. Automation profiles intentionally retain their complete list
/// for cross-repository work.
///
/// An interactive session receives its own repository unconditionally. Such a
/// session already selects the launching user's Account PAT first, which
/// reaches every repository that user can write; gating the narrower, audited,
/// per-repository App token on a profile allowlist only ever withheld the
/// safer credential for the one repository the session was created in.
/// Expanding *beyond* it still needs a human decision or a matching pattern.
pub fn session_github_repositories(
    class: &str,
    configured: &[String],
    current_repo: Option<&str>,
) -> Vec<String> {
    if class != "interactive" {
        return configured.to_vec();
    }
    let mut stamped: Vec<String> = current_repo.map(str::to_string).into_iter().collect();
    stamped.extend(repository_patterns(configured));
    stamped
}

/// A GitHub repository may be launched only when Loom can provide either the
/// launching user's stored PAT or the selected profile's approved App access.
pub async fn github_credential_available(
    db: &Db,
    created_by: Option<&str>,
    github_app: Option<&crate::github_app::GithubApp>,
    allow_user_token: bool,
) -> anyhow::Result<bool> {
    if allow_user_token {
        if let Some(username) = created_by {
            if crate::user_token::get(db, username)
                .await?
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
            {
                return Ok(true);
            }
        }
    }
    Ok(if let Some(app) = github_app {
        app.is_configured().await
    } else {
        false
    })
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

    #[tokio::test]
    async fn launch_environment_respects_restricted_and_strict_profiles() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::profile::env_set(&db, "github_comment", "ANTHROPIC_API_KEY", "profile-token")
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
            vec![("ANTHROPIC_API_KEY".to_string(), "profile-token".to_string())]
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

        let legacy_snapshot = layer_launch_environment(
            &db,
            repo,
            &weaver_core::repo_config::RepoConfig::default(),
            "default",
            vec![
                ("GH_TOKEN".to_string(), "legacy-profile-token".to_string()),
                ("GITHUB_TOKEN".to_string(), "legacy-alias".to_string()),
            ],
            false,
            false,
        )
        .await;
        assert!(legacy_snapshot
            .iter()
            .all(|(name, _)| { !matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN") }));
    }

    #[tokio::test]
    async fn only_the_loom_stored_user_token_is_overlaid() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        seed_user(&db, "bob").await;
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();

        let mut alice = vec![("FOO".to_string(), "bar".to_string())];
        assert!(apply_user_github_token(&db, &mut alice, Some("alice")).await);
        assert!(alice
            .iter()
            .any(|(name, value)| name == "GH_TOKEN" && value == "ghp_alice"));

        let mut bob = Vec::new();
        assert!(!apply_user_github_token(&db, &mut bob, Some("bob")).await);
        assert!(!bob.iter().any(|(name, _)| name == "GH_TOKEN"));
    }

    #[tokio::test]
    async fn github_auth_mode_is_owned_by_the_resolved_session_policy() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = crate::github_app::GithubApp::new(db.clone());

        let mut direct = vec![
            ("GH_TOKEN".to_string(), "loom-stored-user-token".to_string()),
            ("GITHUB_TOKEN".to_string(), "also-wrong".to_string()),
        ];
        assert_eq!(
            stamp_github_auth_mode(&mut direct, Some(&app), false, true).await,
            GithubAuthMode::Direct
        );
        assert!(direct
            .iter()
            .any(|(name, value)| { name == "GH_TOKEN" && value == "loom-stored-user-token" }));
        assert!(direct
            .iter()
            .any(|(name, value)| name == "GITHUB_TOKEN" && value.is_empty()));

        let mut unavailable = vec![
            ("GH_TOKEN".to_string(), "unmanaged-token".to_string()),
            ("GITHUB_TOKEN".to_string(), "also-wrong".to_string()),
        ];
        assert_eq!(
            stamp_github_auth_mode(&mut unavailable, Some(&app), false, false).await,
            GithubAuthMode::Disabled
        );
        assert!(unavailable.iter().all(|(name, value)| {
            !matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN") || value.is_empty()
        }));

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
        let mut brokered = vec![("GITHUB_TOKEN".to_string(), "wrong-daemon-bot".to_string())];
        assert_eq!(
            stamp_github_auth_mode(&mut brokered, Some(&app), false, false).await,
            GithubAuthMode::Broker
        );
        for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
            assert!(brokered
                .iter()
                .any(|(candidate, value)| candidate == name && value.is_empty()));
        }

        let mut restricted = vec![
            ("GH_TOKEN".to_string(), "must-not-win".to_string()),
            ("GITHUB_TOKEN".to_string(), "also-must-not-win".to_string()),
        ];
        assert_eq!(
            stamp_github_auth_mode(&mut restricted, Some(&app), true, true).await,
            GithubAuthMode::Disabled
        );
        assert!(restricted.iter().all(|(name, value)| {
            !matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN") || value.is_empty()
        }));
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
        // The session's own repository no longer has to appear in the profile
        // allowlist: it is the baseline scope for the repository the session
        // was created in.
        assert_eq!(
            session_github_repositories("interactive", &configured, Some("marin-community/loom")),
            ["marin-community/loom"]
        );
        assert!(session_github_repositories("interactive", &configured, None).is_empty());
        assert!(session_github_repositories("interactive", &[], None).is_empty());
        assert_eq!(
            session_github_repositories("automation", &configured, None),
            configured
        );
        assert!(user_github_token_allowed("interactive", false));
        assert!(!user_github_token_allowed("automation", false));
        assert!(!user_github_token_allowed("interactive", true));
    }

    #[test]
    fn owner_patterns_travel_with_the_session_but_never_scope_a_token() {
        let configured = vec![
            "marin-community/*".to_string(),
            "marin-community/marin".to_string(),
        ];
        // An interactive session carries its own repository plus the patterns
        // that let it expand without a human.
        assert_eq!(
            session_github_repositories("interactive", &configured, Some("marin-community/loom")),
            ["marin-community/loom", "marin-community/*"]
        );
        assert_eq!(
            session_github_repositories("automation", &configured, None),
            configured
        );

        let stamped = session_github_repositories("interactive", &configured, Some("owner/repo"));
        assert_eq!(concrete_repositories(&stamped), ["owner/repo"]);
        assert_eq!(repository_patterns(&stamped), ["marin-community/*"]);

        assert_eq!(pattern_owner("marin-community/*"), Some("marin-community"));
        assert_eq!(pattern_owner("marin-community/marin"), None);
        assert_eq!(pattern_owner("/*"), None);
        assert!(is_repository_pattern("marin-community/*"));
        assert!(!is_repository_pattern("marin-community/marin"));
        assert!(!is_repository_pattern("/*"));

        // An allowlist of patterns alone names no repository, so it selects no
        // App credential.
        assert!(scopes_an_app_token(&stamped));
        assert!(!scopes_an_app_token(&["marin-community/*".to_string()]));
        assert!(!scopes_an_app_token(&[]));

        assert!(pattern_allows(&configured, "marin-community/vllm"));
        // Owners are case-insensitive on GitHub.
        assert!(pattern_allows(&configured, "Marin-Community/vllm"));
        assert!(!pattern_allows(&configured, "Open-Athena/marinmirror"));
        // An exact entry is not a pattern, so it grants no wildcard expansion.
        assert!(!pattern_allows(
            &["marin-community/marin".to_string()],
            "marin-community/vllm"
        ));
        assert!(!pattern_allows(&configured, "no-slash"));
    }

    #[tokio::test]
    async fn github_preflight_accepts_user_token_or_configured_app() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        assert!(!github_credential_available(&db, Some("alice"), None, true)
            .await
            .unwrap());
        crate::user_token::set(&db, "alice", "ghp_alice")
            .await
            .unwrap();
        assert!(github_credential_available(&db, Some("alice"), None, true)
            .await
            .unwrap());
        assert!(
            !github_credential_available(&db, Some("alice"), None, false)
                .await
                .unwrap()
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
                    Some("configured-for-preflight".to_string()),
                ),
            ],
        )
        .await
        .unwrap();
        let app = crate::github_app::GithubApp::new(db.clone());
        assert!(github_credential_available(&db, None, Some(&app), false)
            .await
            .unwrap());
    }
}
