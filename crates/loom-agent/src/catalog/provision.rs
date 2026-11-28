//! Boot-time provisioning of Opencode's shared credential store.
//!
//! Opencode keeps credentials in its own database under the shared home, so
//! a deploy cannot hand them to sessions as environment variables. Instead the
//! operator ships the `opencode auth export` JSON through
//! `OPENCODE_CREDENTIALS_JSON` (a loom.toml field rendered into the deploy
//! environment), and the server imports it into the store once per boot —
//! before the first catalogue probe, so the probe and every session launched
//! afterwards see the credentials.
//!
//! Provisioning must never cost a working login, so the declared set is first
//! validated in full and compared with the live store: an already-current
//! store is left untouched, and if the import does not take effect the
//! pre-change store is re-imported and the failure is logged.

use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::db::Db;
use loom_ctx::agent_kind::BuiltinAgentKind;

/// The deploy variable carrying the credential JSON. A loom.toml secret
/// (`opencode_credentials_json`) that `loom config render-env` writes into the
/// deploy `.env`; the server reads it from its environment at boot.
pub const CREDENTIALS_ENV: &str = "OPENCODE_CREDENTIALS_JSON";

/// Bound on one provisioning CLI call. A hung `opencode` (locked store, stalled
/// network) must not wedge the catalogue boot pass or its tick loop.
const CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Import the configured credentials into Opencode's store, once per boot.
///
/// The declared array is authoritative for the integrations it names: when the
/// store already carries those exact credentials (by key, active) nothing
/// happens; otherwise the integrations' existing credentials are removed and
/// the declared set imported. Anything that goes wrong — a malformed payload,
/// a failing CLI, an import that silently takes no effect — warns and leaves
/// the pre-change store in place rather than a half-provisioned one.
pub async fn import_opencode_credentials(db: &Db, configured: Option<&str>) {
    let Some(configured) = configured else {
        return;
    };
    if configured.trim().is_empty() {
        return;
    }
    if !crate::harnesses::builtin_for(BuiltinAgentKind::OpenCode)
        .available()
        .await
    {
        tracing::warn!(
            "{CREDENTIALS_ENV} is set but the opencode CLI is not installed; \
             credentials not imported"
        );
        return;
    }
    let env = crate::agent_env::pairs(db).await.unwrap_or_default();
    let exported = match run_opencode(&["auth", "export", "--standalone"], None, &env).await {
        Ok(output) => output.stdout,
        Err(error) => {
            tracing::warn!(
                "Opencode credentials not checked in: could not read the current store: {error:#}"
            );
            return;
        }
    };
    if !exported.trim().is_empty() && serde_json::from_str::<Vec<Value>>(&exported).is_err() {
        // Treating an unreadable store as empty would let the logout step run
        // blind; leave it alone and report.
        tracing::warn!(
            "Opencode credentials not checked in: the current store export could not be parsed"
        );
        return;
    }
    let plan = match import_plan(configured, &exported) {
        Ok(plan) => plan,
        Err(error) => {
            tracing::warn!("Opencode credentials not imported: {error:#}");
            return;
        }
    };
    if plan.current {
        tracing::info!("Opencode credentials already current; store left untouched");
        return;
    }
    for (integration, id) in &plan.logouts {
        if let Err(error) = run_opencode(
            &["auth", "logout", integration, id, "--standalone"],
            None,
            &env,
        )
        .await
        {
            // The credential may already be gone; the import below still runs.
            tracing::debug!("opencode auth logout {integration} {id} failed: {error}");
        }
    }
    let summary =
        match run_opencode(&["auth", "import", "--standalone"], Some(configured), &env).await {
            Ok(output) => output.summary(),
            Err(error) => {
                tracing::warn!("Opencode credentials not imported: {error:#}");
                restore(&plan.restore, &plan.declared_integrations, &env).await;
                return;
            }
        };
    // `auth import` can exit successfully while storing nothing (the CLI
    // reports to stderr and the store may be busy at boot), so trust the store,
    // not the exit status, before deciding the login survived.
    let after = match run_opencode(&["auth", "export", "--standalone"], None, &env).await {
        Ok(output) => output.stdout,
        Err(error) => {
            tracing::warn!("Opencode credentials not verified: {error:#}");
            restore(&plan.restore, &plan.declared_integrations, &env).await;
            return;
        }
    };
    if !declared_is_current(configured, &after) {
        tracing::warn!(
            "Opencode credential import did not take effect ({summary}); \
             restoring the previous store"
        );
        restore(&plan.restore, &plan.declared_integrations, &env).await;
        return;
    }
    tracing::info!(
        superseded = plan.logouts.len(),
        %summary,
        "provisioned Opencode credentials"
    );
}

/// Which existing credentials stand in the declared set's way, or that the
/// store already carries the declared set.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportPlan {
    /// `(integration, credential id)` pairs to remove before importing.
    logouts: Vec<(String, String)>,
    /// The live store already carries every declared credential, active.
    current: bool,
    /// The pre-change store, re-imported if provisioning fails.
    restore: String,
    /// Integrations the declared set owns.
    declared_integrations: Vec<String>,
}

/// Derive the plan from the declared credentials and the store's current
/// contents (`opencode auth export` output).
fn import_plan(configured: &str, exported: &str) -> Result<ImportPlan> {
    let declared = parse_declared(configured)?;
    let mut declared_integrations: Vec<String> = declared
        .iter()
        .filter_map(|entry| entry.get("integrationID").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    declared_integrations.sort();
    declared_integrations.dedup();
    let existing: Vec<Value> = serde_json::from_str(exported).unwrap_or_default();
    let current = declared_is_current(configured, exported);
    let logouts = existing
        .iter()
        .filter_map(|entry| {
            let integration = entry.get("integrationID").and_then(Value::as_str)?;
            if !declared_integrations.iter().any(|own| own == integration) {
                return None;
            }
            let id = entry.get("id").and_then(Value::as_str)?;
            Some((integration.to_string(), id.to_string()))
        })
        .collect();
    Ok(ImportPlan {
        logouts,
        current,
        restore: exported.to_string(),
        declared_integrations,
    })
}

/// Whether every declared credential is present in the store, active.
fn declared_is_current(configured: &str, exported: &str) -> bool {
    let Ok(declared) = parse_declared(configured) else {
        return false;
    };
    let existing: Vec<Value> = serde_json::from_str(exported).unwrap_or_default();
    declared.iter().all(|entry| {
        let integration = entry.get("integrationID").and_then(Value::as_str);
        let key = entry.get("value").and_then(credential_key);
        existing.iter().any(|current| {
            current.get("integrationID").and_then(Value::as_str) == integration
                && current.get("value").and_then(credential_key) == key
                && current.get("active").and_then(Value::as_bool) == Some(true)
        })
    })
}

/// Validate the declared array before anything is removed from the store: an
/// entry without an id, integration, or usable secret could only ever leave a
/// dead credential behind. One credential per integration keeps the boot
/// deterministic, since only one can be active.
fn parse_declared(configured: &str) -> Result<Vec<Value>> {
    let entries: Vec<Value> = serde_json::from_str(configured)
        .with_context(|| format!("{CREDENTIALS_ENV} must be an opencode auth export JSON array"))?;
    if entries.is_empty() {
        bail!("{CREDENTIALS_ENV} names no credentials");
    }
    let mut integrations: Vec<&str> = Vec::new();
    for entry in &entries {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("every credential needs a non-empty id")?;
        let integration = entry
            .get("integrationID")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .with_context(|| format!("credential '{id}' needs a non-empty integrationID"))?;
        if entry.get("active").and_then(Value::as_bool) == Some(false) {
            bail!(
                "credential '{id}' is marked inactive; only an active credential can be \
                 provisioned"
            );
        }
        if integrations.contains(&integration) {
            bail!(
                "credential '{id}' repeats integration '{integration}'; only one credential per \
                 integration can be active"
            );
        }
        integrations.push(integration);
        let value = entry
            .get("value")
            .filter(|value| value.is_object())
            .with_context(|| format!("credential '{id}' needs a value object"))?;
        if credential_key(value).is_none() {
            bail!(
                "credential '{id}' needs a non-empty value.key (type key) or value.access \
                 (type oauth)"
            );
        }
    }
    Ok(entries)
}

/// The secret a stored credential carries.
fn credential_key(value: &Value) -> Option<&str> {
    let secret = match value.get("type")?.as_str()? {
        "key" => value.get("key"),
        "oauth" => value.get("access"),
        _ => None,
    }?;
    secret.as_str().filter(|secret| !secret.trim().is_empty())
}

/// Put the pre-change store back after a failed provisioning attempt: clear
/// whatever the declared integrations hold now, then re-import the backup.
async fn restore(backup: &str, declared_integrations: &[String], env: &[(String, String)]) {
    let current = match run_opencode(&["auth", "export", "--standalone"], None, env).await {
        Ok(output) => output.stdout,
        Err(error) => {
            tracing::warn!("could not read the store to restore Opencode credentials: {error:#}");
            return;
        }
    };
    if !current.trim().is_empty() && serde_json::from_str::<Vec<Value>>(&current).is_err() {
        tracing::warn!(
            "could not restore the previous Opencode credentials: the current store export \
             could not be parsed"
        );
        return;
    }
    let existing: Vec<Value> = serde_json::from_str(&current).unwrap_or_default();
    for entry in &existing {
        let Some(integration) = entry.get("integrationID").and_then(Value::as_str) else {
            continue;
        };
        if !declared_integrations
            .iter()
            .any(|declared| declared == integration)
        {
            continue;
        }
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let _ = run_opencode(
            &["auth", "logout", integration, id, "--standalone"],
            None,
            env,
        )
        .await;
    }
    let backup_entries: Vec<Value> = serde_json::from_str(backup).unwrap_or_default();
    if backup_entries.is_empty() {
        return;
    }
    match run_opencode(&["auth", "import", "--standalone"], Some(backup), env).await {
        Ok(output) => tracing::info!(
            "restored the previous Opencode credentials: {}",
            output.summary()
        ),
        Err(error) => {
            tracing::warn!("could not restore the previous Opencode credentials: {error:#}")
        }
    }
}

struct CliOutput {
    stdout: String,
    stderr: String,
}

impl CliOutput {
    /// What the CLI said, across both streams — `auth import` reports its
    /// result on stderr, so stdout alone reads as an empty success.
    fn summary(&self) -> String {
        [self.stdout.trim(), self.stderr.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Run one `opencode auth` subcommand against a private server (`--standalone`:
/// the background service is machine-global by port, and the import must
/// reach this loom's own home), optionally feeding `stdin`.
async fn run_opencode(
    args: &[&str],
    stdin: Option<&str>,
    env: &[(String, String)],
) -> Result<CliOutput> {
    let mut command = tokio::process::Command::new("opencode");
    command
        .args(args)
        .envs(env.iter().cloned())
        // A timed-out call drops the child; do not leave the process running.
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn().context("spawning the opencode CLI")?;
    if let Some(payload) = stdin {
        let mut handle = child.stdin.take().context("capturing the import's stdin")?;
        tokio::time::timeout(CLI_TIMEOUT, handle.write_all(payload.as_bytes()))
            .await
            .context("writing the credential payload timed out")?
            .context("writing the credential payload")?;
        // Dropping the handle closes the pipe, so the import reads to EOF.
    }
    let output = tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output())
        .await
        .with_context(|| {
            format!(
                "opencode {} timed out after {}s",
                args.join(" "),
                CLI_TIMEOUT.as_secs()
            )
        })?
        .context("waiting for the opencode CLI")?;
    if !output.status.success() {
        bail!(
            "opencode {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(CliOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARED: &str = r#"[{"id":"cred_new","integrationID":"lunaroute","label":"API key",
        "active":true,"value":{"type":"key","key":"lr_new"}}]"#;

    fn stored(id: &str, key: &str, active: bool) -> String {
        serde_json::json!([{
            "id": id,
            "integrationID": "lunaroute",
            "label": "API key",
            "active": active,
            "value": {"type": "key", "key": key},
        }])
        .to_string()
    }

    #[test]
    fn an_identical_store_is_already_current() {
        let plan = import_plan(DECLARED, &stored("cred_old", "lr_new", true)).unwrap();
        assert!(plan.current, "same integration and key means nothing to do");
    }

    #[test]
    fn a_rotated_key_supersedes_the_old_credential() {
        let plan = import_plan(DECLARED, &stored("cred_old", "lr_old", true)).unwrap();
        assert!(!plan.current);
        assert_eq!(plan.logouts, vec![("lunaroute".into(), "cred_old".into())]);
    }

    #[test]
    fn an_inactive_match_is_not_current() {
        let plan = import_plan(DECLARED, &stored("cred_old", "lr_new", false)).unwrap();
        assert!(!plan.current);
    }

    #[test]
    fn other_integrations_are_left_alone() {
        let exported = serde_json::json!([
            {"id": "o", "integrationID": "opencode", "active": true,
             "value": {"type": "key", "key": "sk_x"}},
            {"id": "l", "integrationID": "lunaroute", "active": true,
             "value": {"type": "key", "key": "lr_old"}},
        ])
        .to_string();
        let plan = import_plan(DECLARED, &exported).unwrap();
        assert_eq!(plan.logouts, vec![("lunaroute".into(), "l".into())]);
    }

    #[test]
    fn a_malformed_declaration_is_rejected_before_anything_changes() {
        assert!(import_plan("{}", "[]").is_err());
        assert!(import_plan("[]", "[]").is_err());
        // Missing the secret a credential could never authenticate with.
        for bad in [
            r#"[{"id":"a","integrationID":"x"}]"#,
            r#"[{"id":"a","integrationID":"x","value":{"type":"key"}}]"#,
            r#"[{"id":"a","integrationID":"x","value":{"type":"key","key":""}}]"#,
            r#"[{"id":"a","integrationID":"x","value":{"type":"oauth"}}]"#,
            r#"[{"id":"a","integrationID":"x","value":{"type":"bogus","key":"k"}}]"#,
            // One active credential per integration.
            r#"[{"id":"a","integrationID":"x","value":{"type":"key","key":"1"}},
                {"id":"b","integrationID":"x","value":{"type":"key","key":"2"}}]"#,
            // An inactive declaration could only be restored on every boot.
            r#"[{"id":"a","integrationID":"x","active":false,
                 "value":{"type":"key","key":"k"}}]"#,
        ] {
            assert!(import_plan(bad, "[]").is_err(), "accepted: {bad}");
        }
    }

    /// A fake `opencode` CLI whose `import` stores the payload it is fed, so a
    /// follow-up `export` reflects the imported state exactly like the real
    /// database does.
    struct FakeCli {
        dir: tempfile::TempDir,
    }

    impl FakeCli {
        fn new(initial_state: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let state = dir.path().join("state.json");
            std::fs::write(&state, initial_state).unwrap();
            let bin = dir.path().join("opencode");
            std::fs::write(
                &bin,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                     case \"$2\" in\n\
                       export) cat {state} ;;\n\
                       import) cat > {state}; printf 'Imported credentials' 1>&2 ;;\n\
                     esac\n",
                    log = dir.path().join("calls.log").display(),
                    state = state.display(),
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { dir }
        }

        fn calls(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
        }

        fn state(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("state.json")).unwrap_or_default()
        }

        /// Point PATH at the fake and set the declared payload for the test.
        fn activate(&self) -> EnvGuard {
            EnvGuard::set(&[("PATH", &self.prefixed_path())])
        }

        fn prefixed_path(&self) -> String {
            format!(
                "{}:{}",
                self.dir.path().display(),
                std::env::var("PATH").unwrap_or_default()
            )
        }
    }

    /// Restores environment variables when the test ends, including on panic,
    /// so a failing assertion cannot change another test's environment.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &str)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var_os(name);
                    std::env::set_var(name, value);
                    (*name, previous)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, previous) in self.saved.drain(..) {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_current_store_is_not_touched() {
        let fake = FakeCli::new(&stored("cred_old", "lr_new", true));
        let _env = fake.activate();
        let db = crate::db::connect_in_memory().await.unwrap();
        import_opencode_credentials(&db, Some(DECLARED)).await;
        let calls = fake.calls();
        assert!(calls.contains("auth export"), "{calls}");
        assert!(
            !calls.contains("auth logout") && !calls.contains("auth import"),
            "an already-current store must not be rewritten: {calls}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_rotation_logs_out_then_imports_and_verifies() {
        let fake = FakeCli::new(&stored("cred_old", "lr_old", true));
        let _env = fake.activate();
        let db = crate::db::connect_in_memory().await.unwrap();
        import_opencode_credentials(&db, Some(DECLARED)).await;
        let calls = fake.calls();
        assert!(
            calls.contains("auth logout lunaroute cred_old --standalone"),
            "{calls}"
        );
        assert!(calls.contains("auth import --standalone"), "{calls}");
        assert_eq!(fake.state(), DECLARED, "the declared set is the store now");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_failed_import_restores_the_previous_store() {
        // The import exits successfully but stores nothing — the case the CLI's
        // exit status hides and the verification re-export catches.
        let fake = FakeCli::new(&stored("cred_old", "lr_old", true));
        let bin = fake.dir.path().join("opencode");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                 case \"$2\" in\n\
                   export) cat {state} ;;\n\
                   import) cat > /dev/null; printf 'Imported 0 credentials' 1>&2 ;;\n\
                 esac\n",
                log = fake.dir.path().join("calls.log").display(),
                state = fake.dir.path().join("state.json").display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _env = fake.activate();
        let db = crate::db::connect_in_memory().await.unwrap();
        import_opencode_credentials(&db, Some(DECLARED)).await;
        let calls = fake.calls();
        assert!(
            calls.matches("auth import --standalone").count() >= 2,
            "the pre-change store is re-imported: {calls}"
        );
    }
}
