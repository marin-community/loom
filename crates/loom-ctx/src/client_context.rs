//! Named remote-client contexts for the `loom` CLI.
//!
//! Server URLs live in the user's XDG config directory. Bearer tokens use a
//! separate owner-only file, while a repository may select a context by name
//! without supplying an endpoint or credential.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "config.toml";
const CREDENTIALS_FILE: &str = "credentials.toml";
const REPO_CONFIG: &str = ".loom/client.toml";

#[derive(Clone, Default, Serialize, Deserialize)]
/// Public endpoint configuration stored in the user's XDG config directory.
pub struct ClientConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_context: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, ContextConfig>,
}

#[derive(Clone, Serialize, Deserialize)]
/// One named Loom server endpoint.
pub struct ContextConfig {
    pub url: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Credentials {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    contexts: BTreeMap<String, ContextCredential>,
}

#[derive(Serialize, Deserialize)]
struct ContextCredential {
    token: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoConfig {
    context: String,
}

#[derive(Clone)]
/// Paths to the public endpoint config and private credential store.
pub struct ClientPaths {
    pub config: PathBuf,
    pub credentials: PathBuf,
}

#[derive(Clone)]
/// A named context resolved for the current invocation.
pub struct ResolvedContext {
    pub name: String,
    pub url: String,
    pub token: Option<String>,
    pub source: ContextSource,
}

#[derive(Clone)]
/// The selector that chose a named context.
pub enum ContextSource {
    Explicit,
    Environment,
    Repository(PathBuf),
    Default,
}

/// Non-secret context metadata displayed by `loom context ls`.
pub struct ContextSummary {
    pub name: String,
    pub url: String,
    pub authenticated: bool,
    pub is_default: bool,
}

impl ClientPaths {
    /// Locate Loom client files using `XDG_CONFIG_HOME` or `~/.config`.
    pub fn discover() -> Result<Self> {
        let root = match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            Some(path) => PathBuf::from(path).join("loom"),
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .context("HOME is required to locate Loom client configuration")?,
            )
            .join(".config/loom"),
        };
        Ok(Self {
            config: root.join(CONFIG_FILE),
            credentials: root.join(CREDENTIALS_FILE),
        })
    }
}

/// Validate and normalize a root HTTP(S) Loom server URL.
pub fn normalize_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(value).context("server URL must include http:// or https://")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("server URL must use http or https and include a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("server URL must not contain credentials");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || !matches!(parsed.path(), "" | "/")
    {
        bail!("server URL must not contain a path, query, or fragment");
    }
    Ok(value.to_string())
}

/// Validate a portable context name and return its trimmed form.
pub fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("context names use 1-64 ASCII letters, digits, '.', '_', or '-'");
    }
    Ok(name)
}

/// Load the public context configuration, or an empty configuration if absent.
pub fn load_config(paths: &ClientPaths) -> Result<ClientConfig> {
    load_toml(&paths.config, "Loom client config")
}

/// Return the configured URL for a named context.
pub fn context_url(paths: &ClientPaths, name: &str) -> Result<Option<String>> {
    let name = validate_name(name)?;
    Ok(load_config(paths)?
        .contexts
        .get(name)
        .map(|context| context.url.clone()))
}

/// Add or update a context endpoint without changing its credential.
pub fn save_context(paths: &ClientPaths, name: &str, url: &str, make_default: bool) -> Result<()> {
    let name = validate_name(name)?.to_string();
    let url = normalize_url(url)?;
    let mut config = load_config(paths)?;
    config.contexts.insert(name.clone(), ContextConfig { url });
    if make_default || config.default_context.is_none() {
        config.default_context = Some(name);
    }
    save_config(paths, &config)
}

/// Save a validated endpoint and its personal API token in separate files.
pub fn save_login(paths: &ClientPaths, name: &str, url: &str, token: &str) -> Result<()> {
    let name = validate_name(name)?.to_string();
    let token = token.trim();
    if token.is_empty() {
        bail!("token must not be empty");
    }
    save_context(paths, &name, url, true)?;
    let mut credentials = load_credentials(paths)?;
    credentials.contexts.insert(
        name,
        ContextCredential {
            token: token.to_string(),
        },
    );
    save_credentials(paths, &credentials)
}

/// Make an existing named context the user default.
pub fn use_context(paths: &ClientPaths, name: &str) -> Result<()> {
    let name = validate_name(name)?;
    let mut config = load_config(paths)?;
    if !config.contexts.contains_key(name) {
        bail!("unknown Loom context '{name}'");
    }
    config.default_context = Some(name.to_string());
    save_config(paths, &config)
}

/// Remove a locally saved credential without revoking the server-side token.
pub fn remove_login(paths: &ClientPaths, name: &str) -> Result<bool> {
    let name = validate_name(name)?;
    let mut credentials = load_credentials(paths)?;
    let removed = credentials.contexts.remove(name).is_some();
    if removed {
        save_credentials(paths, &credentials)?;
    }
    Ok(removed)
}

/// Remove a context and its locally saved credential.
pub fn remove_context(paths: &ClientPaths, name: &str) -> Result<bool> {
    let name = validate_name(name)?;
    let mut config = load_config(paths)?;
    let removed = config.contexts.remove(name).is_some();
    if !removed {
        return Ok(false);
    }
    if config.default_context.as_deref() == Some(name) {
        config.default_context = None;
    }
    save_config(paths, &config)?;
    let _ = remove_login(paths, name)?;
    Ok(true)
}

/// List contexts with credential presence but never credential values.
pub fn list_contexts(paths: &ClientPaths) -> Result<Vec<ContextSummary>> {
    let config = load_config(paths)?;
    let credentials = load_credentials(paths)?;
    Ok(config
        .contexts
        .into_iter()
        .map(|(name, context)| ContextSummary {
            authenticated: credentials.contexts.contains_key(&name),
            is_default: config.default_context.as_deref() == Some(name.as_str()),
            name,
            url: context.url,
        })
        .collect())
}

/// Resolve a named context for the current process and working directory.
pub fn resolve(explicit: Option<&str>) -> Result<Option<ResolvedContext>> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let environment = std::env::var("LOOM_CONTEXT").ok();
    let paths = match ClientPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            let named_context_requested = explicit.is_some_and(|name| !name.trim().is_empty())
                || environment
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
                || repo_context(&cwd)?.is_some();
            if named_context_requested {
                return Err(error);
            }
            return Ok(None);
        }
    };
    resolve_from(&paths, explicit, environment.as_deref(), &cwd)
}

/// Resolve a named context from supplied paths, selectors, and working directory.
pub fn resolve_from(
    paths: &ClientPaths,
    explicit: Option<&str>,
    environment: Option<&str>,
    cwd: &Path,
) -> Result<Option<ResolvedContext>> {
    let config = load_config(paths)?;
    let selected = if let Some(name) = explicit.filter(|name| !name.trim().is_empty()) {
        Some((name.to_string(), ContextSource::Explicit))
    } else if let Some(name) = environment.filter(|name| !name.trim().is_empty()) {
        Some((name.to_string(), ContextSource::Environment))
    } else if let Some((name, path)) = repo_context(cwd)? {
        Some((name, ContextSource::Repository(path)))
    } else {
        config
            .default_context
            .clone()
            .map(|name| (name, ContextSource::Default))
    };
    let Some((name, source)) = selected else {
        return Ok(None);
    };
    let name = validate_name(&name)?.to_string();
    let context = config.contexts.get(&name).with_context(|| {
        format!(
            "Loom context '{name}' is not defined in {}",
            paths.config.display()
        )
    })?;
    let credentials = load_credentials(paths)?;
    let token = credentials
        .contexts
        .get(&name)
        .map(|credential| credential.token.clone());
    Ok(Some(ResolvedContext {
        name,
        url: normalize_url(&context.url)?,
        token,
        source,
    }))
}

fn repo_context(cwd: &Path) -> Result<Option<(String, PathBuf)>> {
    for directory in cwd.ancestors() {
        let path = directory.join(REPO_CONFIG);
        if path.is_file() {
            let config: RepoConfig = load_toml(&path, "repository Loom client config")?;
            return Ok(Some((validate_name(&config.context)?.to_string(), path)));
        }
        if directory.join(".git").exists() {
            break;
        }
    }
    Ok(None)
}

fn load_toml<T: Default + for<'de> Deserialize<'de>>(path: &Path, description: &str) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {description} at {}", path.display()))
}

fn load_credentials(paths: &ClientPaths) -> Result<Credentials> {
    if !paths.credentials.exists() {
        return Ok(Credentials::default());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&paths.credentials)
            .with_context(|| format!("reading metadata for {}", paths.credentials.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            bail!(
                "{} contains Loom credentials and must have mode 0600",
                paths.credentials.display()
            );
        }
    }
    load_toml(&paths.credentials, "Loom client credentials")
}

fn save_config(paths: &ClientPaths, config: &ClientConfig) -> Result<()> {
    let text = toml::to_string_pretty(config).context("serializing Loom client config")?;
    if let Some(parent) = paths.config.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&paths.config, text)
        .with_context(|| format!("writing {}", paths.config.display()))
}

fn save_credentials(paths: &ClientPaths, credentials: &Credentials) -> Result<()> {
    let text =
        toml::to_string_pretty(credentials).context("serializing Loom client credentials")?;
    crate::envfile::write_private(&paths.credentials, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> ClientPaths {
        ClientPaths {
            config: root.join("config.toml"),
            credentials: root.join("credentials.toml"),
        }
    }

    #[test]
    fn login_separates_endpoint_and_private_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        save_login(
            &paths,
            "production",
            "https://loom.example.com/",
            "loom_secret",
        )
        .unwrap();

        let config = std::fs::read_to_string(&paths.config).unwrap();
        let credentials = std::fs::read_to_string(&paths.credentials).unwrap();
        assert!(config.contains("https://loom.example.com"));
        assert!(!config.contains("loom_secret"));
        assert!(credentials.contains("loom_secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&paths.credentials)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn explicit_context_precedes_repository_and_default_contexts() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        save_context(&paths, "local", "http://127.0.0.1:7878", true).unwrap();
        save_login(
            &paths,
            "production",
            "https://loom.example.com",
            "loom_secret",
        )
        .unwrap();
        use_context(&paths, "local").unwrap();
        let repo = directory.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".loom")).unwrap();
        std::fs::write(repo.join(REPO_CONFIG), "context = \"production\"\n").unwrap();

        let repository = resolve_from(&paths, None, None, &repo).unwrap().unwrap();
        assert_eq!(repository.name, "production");
        assert_eq!(repository.token.as_deref(), Some("loom_secret"));
        assert!(matches!(repository.source, ContextSource::Repository(_)));

        let explicit = resolve_from(&paths, Some("local"), Some("production"), &repo)
            .unwrap()
            .unwrap();
        assert_eq!(explicit.name, "local");
        assert!(matches!(explicit.source, ContextSource::Explicit));
    }

    #[test]
    fn urls_and_context_names_reject_credential_injection() {
        assert!(normalize_url("https://user:secret@example.com").is_err());
        assert!(normalize_url("https://example.com/api").is_err());
        assert!(normalize_url("example.com").is_err());
        assert!(validate_name("../../credentials").is_err());
        assert_eq!(
            normalize_url("http://127.0.0.1:7878/").unwrap(),
            "http://127.0.0.1:7878"
        );
    }
}
