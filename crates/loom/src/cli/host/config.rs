//! `loom config` — the typed `loom.toml` and everything derived from it.
//!
//! `loom.toml` is the shared contract deployment tooling builds against.
//! `set` bypasses it and writes the runtime `settings` table directly, with no
//! server running — what a deploy's boot sequence needs, since it must seed
//! the auth settings before loom starts listening.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

/// Shared `--config` flag: the authored `loom.toml`, the single source of
/// truth every `loom setup` wizard fills in and `loom config` reads from.
#[derive(Args)]
pub struct ConfigPathOpts {
    /// Path to `loom.toml`. Defaults to `./loom.toml`, or `$LOOM_CONFIG`.
    #[arg(long, env = crate::loom_config::CONFIG_ENV_VAR, default_value = crate::loom_config::DEFAULT_PATH)]
    pub config: std::path::PathBuf,
}

/// Subcommands under `loom config` — the typed `loom.toml` and everything
/// rendered/pushed from it. `render-env` and `push-secrets` resolve every
/// field from `loom.toml` *or* a same-named env var (env wins) — set one to
/// override a single invocation without editing the file.
///
/// `set` bypasses `loom.toml` and writes directly to the runtime `settings`
/// table (`weaver_core::config::REGISTRY`); see the module docs.
#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Render `loom.toml` as a dotenv file (e.g. `deploy/standalone/.env`).
    RenderEnv(RenderEnvOpts),
    /// Print each secret field's `ENV_NAME`, one per line.
    SecretNames(ConfigPathOpts),
    /// Push each secret field's value to a secret-manager backend. Never
    /// echoes a value.
    PushSecrets(PushSecretsOpts),
    /// Set a runtime setting directly in the sqlite `settings` table — no
    /// running server needed.
    Set {
        /// Dotted key, e.g. `auth.cookie_secure` (see the settings pane, or
        /// `weaver_core::config::REGISTRY`, for the full list).
        key: String,
        value: String,
    },
}

#[derive(Args)]
pub struct RenderEnvOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
    /// Where to write the rendered dotenv file. `-` writes to stdout instead.
    #[arg(long, default_value = "deploy/standalone/.env")]
    out: String,
}

#[derive(Args)]
pub struct PushSecretsOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
    /// Secret-manager backend to push to.
    #[arg(long, value_enum)]
    backend: SecretBackend,
    /// The GCP project id to push into.
    #[arg(long)]
    project: String,
}

#[derive(Clone, clap::ValueEnum)]
pub enum SecretBackend {
    Gcp,
}

/// The default `loom.toml` path, mirroring [`ConfigPathOpts`]'s clap resolution
/// (`$LOOM_CONFIG`, else `./loom.toml`) for the walkthrough, which takes no flag.
pub(crate) fn default_config_path() -> std::path::PathBuf {
    std::env::var(crate::loom_config::CONFIG_ENV_VAR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(crate::loom_config::DEFAULT_PATH))
}

pub async fn run_config(cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::RenderEnv(opts) => cmd_config_render_env(opts),
        ConfigCmd::SecretNames(opts) => cmd_config_secret_names(opts),
        ConfigCmd::PushSecrets(opts) => cmd_config_push_secrets(opts).await,
        ConfigCmd::Set { key, value } => cmd_config_set(key, value).await,
    }
}

/// `loom config set` — write one runtime setting straight into the sqlite
/// `settings` table, no running server needed. The direct-db counterpart to
/// the settings pane's `settings.patch` against a running daemon.
pub(crate) async fn cmd_config_set(key: String, value: String) -> Result<()> {
    if let Err(why) = weaver_core::config::validate(&key, &value) {
        bail!("{key}: {why}");
    }
    let db = crate::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;
    weaver_core::config::apply(&db, &[(key.clone(), Some(value))])
        .await
        .with_context(|| format!("writing setting '{key}'"))?;
    println!("set {key}");
    Ok(())
}

/// Warn to stderr, naming each field, when an ambient env var silently
/// outranked `loom.toml` for this run — the footgun a deploy workstation hits
/// when an `ANTHROPIC_API_KEY` or similar setting happens to be exported
/// (see `loom_config::resolve_reporting_shadows`).
pub(crate) fn warn_shadowed_env(shadowed: &[&str], config_path: &std::path::Path) {
    for name in shadowed {
        eprintln!(
            "warning: ambient env var {name} overrides the value for {name} already set in {} \
             for this run — that's the value being rendered/pushed. Unset {name}, or edit the \
             file, if that's not what you want.",
            config_path.display()
        );
    }
}

/// `loom config render-env` — resolve `loom.toml` (plus any ambient env
/// override) and write it out as a dotenv file, the only place the
/// field→`ENV_NAME` mapping is applied.
pub(crate) fn cmd_config_render_env(opts: RenderEnvOpts) -> Result<()> {
    let (config, shadowed) = crate::loom_config::resolve_reporting_shadows(&opts.config.config)
        .with_context(|| format!("loading {}", opts.config.config.display()))?;
    warn_shadowed_env(&shadowed, &opts.config.config);
    let rendered = crate::loom_config::render_env(&config);
    if opts.out == "-" {
        print!("{rendered}");
    } else {
        let out = std::path::Path::new(&opts.out);
        crate::envfile::write_private(out, &rendered)
            .with_context(|| format!("writing {}", out.display()))?;
        eprintln!(
            "wrote {} from {}",
            out.display(),
            opts.config.config.display()
        );
    }
    Ok(())
}

/// `loom config secret-names` — the secret fields' `ENV_NAME`s, one per line.
/// Static (drawn from the schema, not from which fields happen to be set) —
/// what a Secret Manager provisioning step names its secrets after.
pub(crate) fn cmd_config_secret_names(opts: ConfigPathOpts) -> Result<()> {
    // Resolved (not just iterated statically) so a malformed loom.toml surfaces
    // here rather than only later, in render-env or push-secrets.
    crate::loom_config::resolve(&opts.config)
        .with_context(|| format!("loading {}", opts.config.display()))?;
    for field in crate::loom_config::FIELDS.iter().filter(|f| f.secret) {
        println!("{}", field.env_name);
    }
    Ok(())
}

/// `loom config push-secrets` — push every set secret field to a Secret
/// Manager backend, secret id == `ENV_NAME`. Values travel over the
/// subprocess's stdin, never a command-line argument or a log line.
pub(crate) async fn cmd_config_push_secrets(opts: PushSecretsOpts) -> Result<()> {
    let (config, shadowed) = crate::loom_config::resolve_reporting_shadows(&opts.config.config)
        .with_context(|| format!("loading {}", opts.config.config.display()))?;
    warn_shadowed_env(&shadowed, &opts.config.config);
    let mut pushed = Vec::new();
    let mut skipped = Vec::new();
    for field in crate::loom_config::FIELDS.iter().filter(|f| f.secret) {
        let Some(value) = field.get(&config) else {
            skipped.push(field.env_name);
            continue;
        };
        match opts.backend {
            SecretBackend::Gcp => gcp_push_secret(&opts.project, field.env_name, value).await,
        }
        .with_context(|| format!("pushing {} to Secret Manager", field.env_name))?;
        pushed.push(field.env_name);
    }
    if !pushed.is_empty() {
        println!("pushed: {}", pushed.join(", "));
    }
    if !skipped.is_empty() {
        println!("skipped (not set in loom.toml): {}", skipped.join(", "));
    }
    Ok(())
}

/// Create-or-update one GCP Secret Manager secret via the `gcloud` CLI,
/// feeding `value` over stdin so it never appears in an argument list or a
/// process listing.
pub(crate) async fn gcp_push_secret(project: &str, name: &str, value: &str) -> Result<()> {
    let exists = tokio::process::Command::new("gcloud")
        .args(["secrets", "describe", name, "--project", project])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("failed to spawn gcloud (is the Google Cloud SDK installed?)")?
        .success();
    let args: &[&str] = if exists {
        &[
            "secrets",
            "versions",
            "add",
            name,
            "--project",
            project,
            "--data-file=-",
        ]
    } else {
        &[
            "secrets",
            "create",
            name,
            "--project",
            project,
            "--replication-policy=automatic",
            "--data-file=-",
        ]
    };
    run_gcloud_with_stdin(args, value).await
}

/// Run `gcloud <args>`, writing `stdin_data` to its stdin and closing it.
pub(crate) async fn run_gcloud_with_stdin(args: &[&str], stdin_data: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("gcloud")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn gcloud (is the Google Cloud SDK installed?)")?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(stdin_data.as_bytes())
        .await
        .context("writing the secret value to gcloud's stdin")?;
    let out = child
        .wait_with_output()
        .await
        .context("waiting for gcloud")?;
    if !out.status.success() {
        bail!(
            "gcloud {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `loom config set` writes straight to the sqlite `settings` table — no
    /// HTTP, no running server.
    #[tokio::test]
    #[serial_test::serial]
    async fn config_set_writes_directly_to_sqlite_with_no_server() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());

        cmd_config_set("auth.cookie_secure".to_string(), "true".to_string())
            .await
            .unwrap();

        let db = crate::db::connect(&weaver_core::db::default_db_path())
            .await
            .unwrap();
        assert_eq!(
            weaver_core::config::get(&db, "auth.cookie_secure")
                .await
                .as_deref(),
            Some("true")
        );

        // An invalid value for a registered (bool) key is rejected before
        // touching the database.
        let err = cmd_config_set("auth.cookie_secure".to_string(), "sideways".to_string())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("auth.cookie_secure"),
            "error should name the key: {err}"
        );

        std::env::remove_var("WEAVER_HOME");
    }
}
