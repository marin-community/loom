//! The `opencode` builtin: Opencode's TUI as the terminal fallback and its
//! built-in ACP server (`opencode acp`). Every ACP session runs against its
//! own Opencode data home — credentials especially — seeded once from the
//! machine's deploy-provisioned store (see [`scope_session_data_home`]); the
//! shared credential store is deploy-provisioned (see `catalog::provision`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use weaver_core::BoxFut;

use super::{
    binary_on_path, join_args, model_flag, push_env_default, spaced, AcpContract, BuiltinHarness,
};
use crate::agent::{
    sh_single_quote, sh_single_quote_path, start_terminal, AgentLaunchContext, AgentMetadata,
    AgentType, LaunchMode,
};
use crate::db::Db;
use loom_ctx::agent_kind::BuiltinAgentKind;

pub static HARNESS: OpenCodeAgentType = OpenCodeAgentType;

pub struct OpenCodeAgentType;

impl AgentType for OpenCodeAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "opencode".to_string(),
            label: "Opencode".to_string(),
            // The picker reads models from the stored catalogue.
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: true,
            supports_hooks: false,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
            modes: self.acp_modes().into_iter().map(str::to_string).collect(),
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Fresh)).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Adopt)).await
        })
    }
}

impl BuiltinHarness for OpenCodeAgentType {
    fn kind(&self) -> BuiltinAgentKind {
        BuiltinAgentKind::OpenCode
    }

    fn credential_names(&self) -> &'static [&'static str] {
        // A credential rotation is applied by the boot import and by the
        // per-pass list/prune; the payload itself is stripped from the
        // environment before fingerprinting, so it cannot be hashed here.
        &["OPENCODE_CONFIG_CONTENT"]
    }

    fn available(&self) -> BoxFut<'static, bool> {
        Box::pin(binary_on_path("opencode"))
    }

    fn acp_command(&self, db: Db) -> BoxFut<'static, String> {
        Box::pin(async move {
            super::acp_cmd_override(&db, "WEAVER_OPENCODE_ACP_CMD", "acp.opencode_cmd")
                .await
                .unwrap_or_else(|| "opencode acp".to_string())
        })
    }

    fn acp_mode_for(&self, mode: &str) -> Option<&'static str> {
        let mode = mode.trim();
        // Opencode ships two agents: `build` runs tools under its own
        // permission prompts, `plan` is read-only. There is no
        // accepted-edits or bypass posture, so neither is offered rather than
        // silently running with different prompts than the name implies.
        Some(match mode {
            "default" | "auto" | "" | "build" => "build",
            "plan" => "plan",
            _ => return None,
        })
    }

    fn auto_approves(&self, _mode: &str) -> bool {
        // Opencode prompts per its own permission config in both of its modes.
        false
    }

    fn seeds_primer_as_goal(&self) -> bool {
        true
    }

    fn acp_meta(&self, _contract: &AcpContract<'_>) -> Option<Value> {
        None
    }

    fn prepare_acp_env(
        &self,
        env: &mut Vec<(String, String)>,
        contract: &AcpContract<'_>,
    ) -> Result<Vec<String>> {
        let session_id = env
            .iter()
            .find(|(name, _)| name == "LOOM_SESSION_ID")
            .map(|(_, value)| value.clone());
        match session_id {
            Some(session_id) => {
                scope_session_data_home(env, &session_id, contract.restricted)?;
            }
            None => tracing::warn!(
                "the opencode session has no LOOM_SESSION_ID; its data home is not isolated"
            ),
        }
        Ok(Vec::new())
    }
}

// -- per-session data home -------------------------------------------------

/// Point one Opencode session at a private data home, seeded from the
/// machine's shared credential store.
///
/// Opencode v2 keeps credentials — the active flag per integration included —
/// in a sqlite database under its data home, so every session sharing that home
/// shares one credential per integration. A private home, keyed by the loom
/// session id, isolates them: a session that logs into another account (an
/// agent running `opencode auth login`, or the operator through the TUI)
/// changes only its own copy, and two profiles can run different accounts by
/// logging their sessions in separately. The first use copies the deploy's
/// provisioned store in (`opencode.db` with its write-ahead log), so a fresh
/// session starts authenticated exactly like the shared home; a home that
/// already holds a store is never re-seeded, so a session's own logins survive
/// reattachment and restarts.
///
/// Restricted sessions are scoped but never seeded: the adapter must not
/// receive the deploy's credentials, and an empty home is the only way
/// Opencode's database-backed store can be kept out of one. The terminal (TUI)
/// fallback keeps the shared home — it is the operator's own interactive use —
/// and the catalogue probe reads the shared home too: the deploy's store is the
/// view the catalogue represents, per-profile views included.
fn scope_session_data_home(
    env: &mut Vec<(String, String)>,
    session_id: &str,
    restricted: bool,
) -> Result<()> {
    if env.iter().any(|(name, _)| name == "XDG_DATA_HOME") {
        // An operator-provided home is a deliberate per-deploy override.
        return Ok(());
    }
    // Session ids are loom-generated, but the value reaches a path join —
    // refuse anything that could name outside the weaver home rather than
    // trust the generator.
    if session_id.is_empty()
        || session_id == "."
        || session_id == ".."
        || session_id.contains(['/', '\\'])
    {
        tracing::warn!(
            session = %session_id,
            "the opencode session id is not a safe directory name; its data home is not isolated"
        );
        return Ok(());
    }
    let data = weaver_core::db::weaver_home()
        .join("opencode-data")
        .join(session_id);
    let store = data.join("opencode");
    if !store.join("opencode.db").is_file() {
        std::fs::create_dir_all(&store)
            .with_context(|| format!("creating the opencode data home {}", store.display()))?;
        if !restricted {
            seed_from_shared_store(&store);
        }
    }
    push_env_default(env, "XDG_DATA_HOME", &data.display().to_string());
    Ok(())
}

/// Copy the machine's provisioned credential database into a fresh session
/// home. The write-ahead log and its index ride along so a store written
/// moments before (the boot import) survives the copy.
fn seed_from_shared_store(store: &Path) {
    let shared = shared_store_dir();
    for name in ["opencode.db", "opencode.db-wal", "opencode.db-shm"] {
        let from = shared.join(name);
        if from.is_file() {
            if let Err(error) = std::fs::copy(&from, store.join(name)) {
                tracing::warn!(
                    %error,
                    source = %from.display(),
                    "could not seed the session's Opencode credential store"
                );
            }
        }
    }
}

/// The machine's own Opencode data directory — the deploy store the boot
/// import provisions — resolved the way Opencode resolves it.
fn shared_store_dir() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("opencode"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".local")
            .join("share")
            .join("opencode"),
    }
}

// -- terminal launch --------------------------------------------------------

/// The Opencode TUI is the terminal fallback for the ACP builtin: `--continue`
/// resumes in an existing worktree, a fresh run preloads the goal via
/// `--prompt`.
pub(crate) fn opencode_command(ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = spaced(join_args([
        model_flag(ctx.model).map(|m| format!("--model {}", sh_single_quote(m)))
    ]));
    match (mode, ctx.goal_file.or(ctx.primer_file)) {
        (LaunchMode::Adopt, _) => format!("opencode --continue{args}"),
        (LaunchMode::Fresh, Some(f)) => {
            format!(
                "opencode{args} --prompt \"$(cat {})\"",
                sh_single_quote_path(f)
            )
        }
        (LaunchMode::Fresh, None) => format!("opencode{args}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores environment variables when the test ends, including on panic,
    /// so a failing assertion cannot change another test's environment.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<std::ffi::OsString>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var_os(name);
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
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

    fn session_env(session_id: &str) -> Vec<(String, String)> {
        vec![("LOOM_SESSION_ID".to_string(), session_id.to_string())]
    }

    /// A deploy store fixture: `opencode.db` plus the write-ahead log a recent
    /// boot import leaves behind.
    fn deploy_store(dir: &Path) {
        let store = dir.join("opencode");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("opencode.db"), b"credentials").unwrap();
        std::fs::write(store.join("opencode.db-wal"), b"recent import").unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_fresh_session_is_scoped_and_seeded_from_the_deploy_store() {
        let home = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        deploy_store(shared.path());
        let _env = EnvGuard::set(&[
            ("WEAVER_HOME", Some(home.path().as_os_str().to_owned())),
            ("XDG_DATA_HOME", Some(shared.path().as_os_str().to_owned())),
        ]);

        let mut env = session_env("acp-opencode-1");
        scope_session_data_home(&mut env, "acp-opencode-1", false).unwrap();

        let scoped = home.path().join("opencode-data/acp-opencode-1");
        assert_eq!(
            env.iter()
                .find(|(name, _)| name == "XDG_DATA_HOME")
                .map(|(_, value)| value.as_str()),
            Some(scoped.display().to_string().as_str()),
            "the session env points at its own data home"
        );
        let store = scoped.join("opencode");
        assert_eq!(
            std::fs::read(store.join("opencode.db")).unwrap(),
            b"credentials",
        );
        assert_eq!(
            std::fs::read(store.join("opencode.db-wal")).unwrap(),
            b"recent import",
            "the write-ahead log rides along so a recent boot import survives"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_restricted_session_is_scoped_but_never_seeded() {
        let home = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        deploy_store(shared.path());
        let _env = EnvGuard::set(&[
            ("WEAVER_HOME", Some(home.path().as_os_str().to_owned())),
            ("XDG_DATA_HOME", Some(shared.path().as_os_str().to_owned())),
        ]);

        let mut env = session_env("acp-restricted");
        scope_session_data_home(&mut env, "acp-restricted", true).unwrap();

        let store = home.path().join("opencode-data/acp-restricted/opencode");
        assert!(
            !store.join("opencode.db").exists(),
            "the deploy's credentials must not reach a restricted adapter"
        );
        assert!(
            env.iter().any(|(name, _)| name == "XDG_DATA_HOME"),
            "the empty home is still scoped, so the shared store stays invisible"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_session_home_that_already_holds_a_store_is_never_reseeded() {
        let home = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        deploy_store(shared.path());
        // The session logged into another account after its first launch: the
        // next attach (or restart) must keep that, not re-import the deploy's.
        let own = home.path().join("opencode-data/acp-logged-in/opencode");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("opencode.db"), b"its own login").unwrap();
        let _env = EnvGuard::set(&[
            ("WEAVER_HOME", Some(home.path().as_os_str().to_owned())),
            ("XDG_DATA_HOME", Some(shared.path().as_os_str().to_owned())),
        ]);

        let mut env = session_env("acp-logged-in");
        scope_session_data_home(&mut env, "acp-logged-in", false).unwrap();

        assert_eq!(
            std::fs::read(own.join("opencode.db")).unwrap(),
            b"its own login",
        );
        assert!(
            !own.join("opencode.db-wal").exists(),
            "nothing from the deploy store is copied over an existing login"
        );
    }

    /// Manual smoke test against the installed opencode CLI: a session home
    /// seeded by [`scope_session_data_home`] must serve exactly the deploy
    /// store's credentials. Run with
    /// `cargo test -p loom-agent --lib live_opencode -- --ignored`.
    #[tokio::test]
    #[ignore]
    #[serial_test::serial]
    async fn live_opencode_session_home_serves_the_deploy_store() {
        let export = |data_home: Option<&std::ffi::OsStr>| {
            let mut command = tokio::process::Command::new("opencode");
            command.args(["auth", "export", "--standalone"]);
            if let Some(dir) = data_home {
                command.env("XDG_DATA_HOME", dir);
            }
            async fn run(
                mut command: tokio::process::Command,
            ) -> std::result::Result<String, String> {
                let output = command
                    .output()
                    .await
                    .map_err(|error| format!("spawning the opencode CLI: {error}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "opencode auth export failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            run(command)
        };

        let machine = export(None).await.expect("the opencode CLI runs");
        assert!(
            machine.trim_start_matches('[') != machine,
            "the machine store has no opencode credentials to seed from; \
             log in or set the deploy credentials before this test"
        );

        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set(&[("WEAVER_HOME", Some(home.path().as_os_str().to_owned()))]);
        let mut env = session_env("live-seed-check");
        scope_session_data_home(&mut env, "live-seed-check", false).unwrap();
        let scoped = env
            .iter()
            .find(|(name, _)| name == "XDG_DATA_HOME")
            .map(|(_, value)| std::ffi::OsString::from(value))
            .expect("the session home is scoped");

        let seeded = export(Some(&scoped))
            .await
            .expect("the seeded home exports");
        assert_eq!(
            seeded, machine,
            "the session home serves exactly the deploy store's credentials"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn an_operator_provided_data_home_wins_over_isolation() {
        let home = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        deploy_store(shared.path());
        let _env = EnvGuard::set(&[
            ("WEAVER_HOME", Some(home.path().as_os_str().to_owned())),
            ("XDG_DATA_HOME", Some(shared.path().as_os_str().to_owned())),
        ]);

        let mut env = session_env("acp-override");
        env.push((
            "XDG_DATA_HOME".to_string(),
            "/srv/opencode-managed".to_string(),
        ));
        scope_session_data_home(&mut env, "acp-override", false).unwrap();

        assert_eq!(
            env.iter()
                .find(|(name, _)| name == "XDG_DATA_HOME")
                .map(|(_, value)| value.as_str()),
            Some("/srv/opencode-managed"),
            "a deliberate per-deploy home is a documented escape hatch"
        );
        assert!(
            !home.path().join("opencode-data").exists(),
            "no private home is created behind the operator's back"
        );
    }
}
