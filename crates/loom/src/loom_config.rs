//! The typed `loom.toml` — the single authored source of truth for every
//! credential and deploy-mechanism setting, secret or not (GitHub App
//! credentials, paste-once agent secrets, and knobs like `LOOM_TLS_EMAIL` or
//! `HOST_UID` alike). [`FIELDS`] is the one place a field's `ENV_NAME` and
//! secret/non-secret marker are spelled — `loom setup` fills a [`LoomConfig`]
//! and calls [`upsert`] to persist it; `loom config
//! render-env`/`secret-names`/`push-secrets` (`bin/loom.rs`) all iterate
//! [`FIELDS`] generically instead of naming an `ENV_NAME` themselves.
//! `deploy/standalone/.env` is generated from this file by [`render_env`],
//! not hand-authored.
//!
//! Every field resolves from **either** `loom.toml` **or** a same-named
//! environment variable — [`resolve`] layers the process environment over
//! whatever's on disk (env wins), the standard "env overrides a config file"
//! expectation (the same reason `GH_TOKEN` in your shell already overrides
//! the `gh` CLI's own stored auth). That layering only happens on the
//! *consuming* path (`render-env`, `push-secrets`) — [`load`]/[`upsert`], the
//! *authoring* path `loom setup` writes through, never touch the
//! environment, so an operator's ambient `GH_TOKEN` (say, for their own `gh`
//! usage) can't silently get baked into the committed `loom.toml`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default path for the authored config file, relative to the repo root.
pub const DEFAULT_PATH: &str = "loom.toml";
/// Env var that overrides [`DEFAULT_PATH`] — read via clap's `env` attribute
/// on every `--config` flag, so every `loom config`/`loom setup` subcommand
/// honors it uniformly.
pub const CONFIG_ENV_VAR: &str = "LOOM_CONFIG";

/// Every credential/setting a deploy needs. Each field is optional — an
/// operator fills them in incrementally (the GitHub App wizard, then `loom
/// setup secrets`, then whatever deploy-mechanism knobs they need), and
/// [`upsert`] preserves whatever is already there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LoomConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_github: Option<String>,
    /// Extra GitHub accounts (orgs or users), comma/space-separated, whose App
    /// installations loom should trust — the bootstrap trusted-owner allowlist.
    /// The deploy `owner_github` is always trusted; this adds the orgs you run
    /// loom for. Keeps a public App from honoring a stranger's installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_owners: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_app_private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_webhook_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_gid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// One field's identity in the shared `loom config` contract: its stable
/// `ENV_NAME`, whether it holds a secret, and typed accessors into
/// [`LoomConfig`]. `render-env`, `secret-names`, and `push-secrets` all drive
/// off this — none of them spells an `ENV_NAME` itself.
pub struct FieldSpec {
    pub env_name: &'static str,
    pub secret: bool,
    get_fn: fn(&LoomConfig) -> Option<&str>,
    set_fn: fn(&mut LoomConfig, String),
}

impl FieldSpec {
    pub fn get<'a>(&self, config: &'a LoomConfig) -> Option<&'a str> {
        (self.get_fn)(config)
    }

    pub fn set(&self, config: &mut LoomConfig, value: String) {
        (self.set_fn)(config, value)
    }
}

/// The full contract, in the stable order `render-env`/`secret-names` emit.
pub static FIELDS: &[FieldSpec] = &[
    FieldSpec {
        env_name: "LOOM_DOMAIN",
        secret: false,
        get_fn: |c| c.domain.as_deref(),
        set_fn: |c, v| c.domain = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_OWNER_GITHUB",
        secret: false,
        get_fn: |c| c.owner_github.as_deref(),
        set_fn: |c, v| c.owner_github = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_ALLOWED_OWNERS",
        secret: false,
        get_fn: |c| c.allowed_owners.as_deref(),
        set_fn: |c, v| c.allowed_owners = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_GITHUB_APP_ID",
        secret: false,
        get_fn: |c| c.github_app_id.as_deref(),
        set_fn: |c, v| c.github_app_id = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_GITHUB_APP_PRIVATE_KEY",
        secret: true,
        get_fn: |c| c.github_app_private_key.as_deref(),
        set_fn: |c, v| c.github_app_private_key = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_GITHUB_WEBHOOK_SECRET",
        secret: true,
        get_fn: |c| c.github_webhook_secret.as_deref(),
        set_fn: |c, v| c.github_webhook_secret = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_GITHUB_CLIENT_ID",
        secret: false,
        get_fn: |c| c.github_client_id.as_deref(),
        set_fn: |c, v| c.github_client_id = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_GITHUB_CLIENT_SECRET",
        secret: true,
        get_fn: |c| c.github_client_secret.as_deref(),
        set_fn: |c, v| c.github_client_secret = Some(v),
    },
    FieldSpec {
        env_name: "ANTHROPIC_API_KEY",
        secret: true,
        get_fn: |c| c.anthropic_api_key.as_deref(),
        set_fn: |c, v| c.anthropic_api_key = Some(v),
    },
    FieldSpec {
        env_name: "GH_TOKEN",
        secret: true,
        get_fn: |c| c.gh_token.as_deref(),
        set_fn: |c, v| c.gh_token = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_TLS_EMAIL",
        secret: false,
        get_fn: |c| c.tls_email.as_deref(),
        set_fn: |c, v| c.tls_email = Some(v),
    },
    FieldSpec {
        env_name: "HOST_UID",
        secret: false,
        get_fn: |c| c.host_uid.as_deref(),
        set_fn: |c, v| c.host_uid = Some(v),
    },
    FieldSpec {
        env_name: "HOST_GID",
        secret: false,
        get_fn: |c| c.host_gid.as_deref(),
        set_fn: |c, v| c.host_gid = Some(v),
    },
    FieldSpec {
        env_name: "LOOM_IMAGE",
        secret: false,
        get_fn: |c| c.image.as_deref(),
        set_fn: |c, v| c.image = Some(v),
    },
];

/// Look up a field by its `ENV_NAME`. Panics on an unknown name — every
/// caller in this codebase passes a name drawn from [`FIELDS`] itself.
fn field(env_name: &str) -> &'static FieldSpec {
    FIELDS
        .iter()
        .find(|f| f.env_name == env_name)
        .unwrap_or_else(|| panic!("unknown loom config field {env_name}"))
}

/// Load `path` as a [`LoomConfig`]; a missing file is an empty (all-`None`)
/// config, not an error — every field is optional and filled in
/// incrementally.
pub fn load(path: &Path) -> Result<LoomConfig> {
    if !path.exists() {
        return Ok(LoomConfig::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// [`load`] `path`, then layer the process environment over the result — for
/// each field, an ambient `ENV_NAME` (if set) overrides whatever `loom.toml`
/// has. What every *consumer* of the config (`render-env`, `push-secrets`)
/// resolves against; see the module docs for why the *authoring* path
/// ([`load`]/[`upsert`]) doesn't do this.
pub fn resolve(path: &Path) -> Result<LoomConfig> {
    Ok(resolve_reporting_shadows(path)?.0)
}

/// [`resolve`], plus the `ENV_NAME`s where an ambient env var overrode a
/// *different* value already present in `path` — the footgun on a deploy
/// workstation, where a personal `GH_TOKEN`/`ANTHROPIC_API_KEY` etc. is
/// commonly exported and would otherwise silently outrank `loom.toml` in
/// exactly the run that pushes secrets to the shared deploy. `render-env` and
/// `push-secrets` (`bin/loom.rs`) use this instead of bare [`resolve`] so they
/// can warn; a value that merely fills in a field the file never had isn't a
/// shadow (nothing on disk was overridden), so it's excluded.
pub fn resolve_reporting_shadows(path: &Path) -> Result<(LoomConfig, Vec<&'static str>)> {
    let mut config = load(path)?;
    let mut shadowed = Vec::new();
    for f in FIELDS {
        if let Ok(value) = std::env::var(f.env_name) {
            if let Some(existing) = f.get(&config) {
                if existing != value {
                    shadowed.push(f.env_name);
                }
            }
            f.set(&mut config, value);
        }
    }
    Ok((config, shadowed))
}

/// Serialize `config` to TOML and write it to `path` (0600 — it can hold
/// secrets), creating parent directories as needed.
pub fn save(path: &Path, config: &LoomConfig) -> Result<()> {
    let text = toml::to_string_pretty(config).context("serializing loom.toml")?;
    crate::envfile::write_private(path, &text)
}

/// Load `path`, set every `(ENV_NAME, value)` pair from `updates` onto it
/// (via each field's [`FieldSpec::set`]), and save it back — fields not named
/// in `updates` are left exactly as they were.
pub fn upsert(path: &Path, updates: &[(&str, &str)]) -> Result<LoomConfig> {
    let mut config = load(path)?;
    for (env_name, value) in updates {
        field(env_name).set(&mut config, value.to_string());
    }
    save(path, &config)?;
    Ok(config)
}

/// Render `config` as dotenv text (the shape `render-env` writes) — every set
/// field as `ENV_NAME=value` in [`FIELDS`] order, reusing
/// [`crate::envfile::upsert`]'s quoting/escaping (multi-line PEM values
/// included) against an empty starting file.
pub fn render_env(config: &LoomConfig) -> String {
    let pairs: Vec<(&str, &str)> = FIELDS
        .iter()
        .filter_map(|f| f.get(config).map(|v| (f.env_name, v)))
        .collect();
    crate::envfile::upsert("", &pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_on_a_missing_file_creates_it_with_only_the_given_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        let config = upsert(&path, &[("LOOM_DOMAIN", "loom.example.com")]).unwrap();
        assert_eq!(config.domain.as_deref(), Some("loom.example.com"));
        assert_eq!(config.owner_github, None);

        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded, config);
    }

    #[test]
    fn upsert_preserves_fields_from_an_earlier_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        upsert(&path, &[("LOOM_DOMAIN", "loom.example.com")]).unwrap();
        let config = upsert(&path, &[("GH_TOKEN", "ghp_new")]).unwrap();
        assert_eq!(config.domain.as_deref(), Some("loom.example.com"));
        assert_eq!(config.gh_token.as_deref(), Some("ghp_new"));
    }

    #[test]
    #[serial_test::serial]
    fn resolve_reporting_shadows_flags_only_a_genuinely_overridden_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        upsert(
            &path,
            &[
                ("LOOM_TLS_EMAIL", "from-file@example.com"),
                ("LOOM_DOMAIN", "loom.example.com"),
            ],
        )
        .unwrap();

        // Shadows the file's value.
        std::env::set_var("LOOM_TLS_EMAIL", "from-env@example.com");
        // Same value as the file — not a shadow, nothing actually changes.
        std::env::set_var("LOOM_DOMAIN", "loom.example.com");
        // Fills in a field the file never had — not a shadow either.
        std::env::set_var("HOST_UID", "1001");

        let (resolved, shadowed) = resolve_reporting_shadows(&path).unwrap();

        std::env::remove_var("LOOM_TLS_EMAIL");
        std::env::remove_var("LOOM_DOMAIN");
        std::env::remove_var("HOST_UID");

        assert_eq!(resolved.tls_email.as_deref(), Some("from-env@example.com"));
        assert_eq!(shadowed, vec!["LOOM_TLS_EMAIL"]);
    }

    #[test]
    #[serial_test::serial]
    fn resolve_lets_an_ambient_env_var_override_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        upsert(&path, &[("LOOM_TLS_EMAIL", "from-file@example.com")]).unwrap();

        std::env::set_var("LOOM_TLS_EMAIL", "from-env@example.com");
        let resolved = resolve(&path).unwrap();
        std::env::remove_var("LOOM_TLS_EMAIL");

        assert_eq!(resolved.tls_email.as_deref(), Some("from-env@example.com"));
    }

    #[test]
    #[serial_test::serial]
    fn resolve_fills_in_a_field_the_file_never_had() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        upsert(&path, &[("LOOM_DOMAIN", "loom.example.com")]).unwrap();

        std::env::set_var("HOST_UID", "1001");
        let resolved = resolve(&path).unwrap();
        std::env::remove_var("HOST_UID");

        assert_eq!(resolved.domain.as_deref(), Some("loom.example.com"));
        assert_eq!(resolved.host_uid.as_deref(), Some("1001"));
    }

    #[test]
    #[serial_test::serial]
    fn ambient_env_never_leaks_into_load_or_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");

        std::env::set_var("LOOM_TLS_EMAIL", "ambient@example.com");
        let config = upsert(&path, &[("LOOM_DOMAIN", "loom.example.com")]).unwrap();
        std::env::remove_var("LOOM_TLS_EMAIL");

        assert_eq!(
            config.tls_email, None,
            "upsert must not pick up ambient env"
        );
        let reloaded = load(&path).unwrap();
        assert_eq!(
            reloaded.tls_email, None,
            "the saved file must not contain it either"
        );
    }

    #[test]
    fn render_env_only_emits_set_fields_and_escapes_the_pem() {
        let config = LoomConfig {
            domain: Some("loom.example.com".to_string()),
            github_app_private_key: Some(
                "-----BEGIN KEY-----\nAAAA\n-----END KEY-----".to_string(),
            ),
            ..Default::default()
        };
        let rendered = render_env(&config);
        assert_eq!(
            rendered,
            "LOOM_DOMAIN=loom.example.com\nLOOM_GITHUB_APP_PRIVATE_KEY=\"-----BEGIN KEY-----\\nAAAA\\n-----END KEY-----\"\n"
        );
    }

    #[test]
    fn field_secret_markers_match_the_shared_contract() {
        let secret_names: Vec<&str> = FIELDS
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.env_name)
            .collect();
        assert_eq!(
            secret_names,
            vec![
                "LOOM_GITHUB_APP_PRIVATE_KEY",
                "LOOM_GITHUB_WEBHOOK_SECRET",
                "LOOM_GITHUB_CLIENT_SECRET",
                "ANTHROPIC_API_KEY",
                "GH_TOKEN",
            ]
        );
    }
}
