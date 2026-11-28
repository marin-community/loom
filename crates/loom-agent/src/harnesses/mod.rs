//! One module per builtin harness.
//!
//! Every fact specific to a harness — its metadata tables, terminal and ACP
//! commands, credential names, mode vocabulary, initialize `_meta` — lives in
//! its own file, and the rest of the codebase iterates [`BuiltinHarness`]
//! rather than naming the harness again. Adding a harness is adding a module
//! and registering it in [`builtin_for`]; no call site changes.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod opencode;

use std::time::Duration;

use loom_ctx::agent_kind::BuiltinAgentKind;
use serde_json::Value;

use crate::agent::AgentType;
use crate::db::Db;
use weaver_core::BoxFut;

/// Loom's launch postures, in the order a reverse lookup prefers.
pub const LOOM_POSTURES: [&str; 5] = [
    "default",
    "auto",
    "acceptEdits",
    "plan",
    "bypassPermissions",
];

/// The same postures in the order the picker offers them.
const PICKER_POSTURES: [&str; 5] = [
    "auto",
    "default",
    "acceptEdits",
    "plan",
    "bypassPermissions",
];

/// The launch facts an [`AcpContract`] hook sees, in the adapter-agnostic
/// vocabulary `build_acp_launch` resolves.
pub struct AcpContract<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    /// The launch posture in Loom's vocabulary, or the adapter's own
    /// persisted id when `mode_from_live` is set.
    pub mode: &'a str,
    pub mode_from_live: bool,
    pub restricted: bool,
    pub allowed_tools_json: &'a str,
    pub primer_text: Option<&'a str>,
}

/// A code-shipped harness: the [`AgentType`] it launches plus the facts the
/// catalogue, web, and ACP layers need without naming the harness again.
pub trait BuiltinHarness: AgentType {
    /// The registry key and wire id.
    fn kind(&self) -> BuiltinAgentKind;

    /// Credential names whose values scope the harness's catalogue rows; a
    /// rotation of any of them forces a full refresh.
    fn credential_names(&self) -> &'static [&'static str] {
        &[]
    }

    /// Extra `initialize` `_meta` the adapter opted into, if any.
    fn initialize_meta(&self) -> Option<Value> {
        None
    }

    /// Whether the harness binary can be probed on this machine.
    fn available(&self) -> BoxFut<'static, bool>;

    /// The ACP adapter command, with the same override precedence as a live
    /// launch (environment, then config setting, then the default).
    fn acp_command(&self, db: Db) -> BoxFut<'static, String>;

    /// The adapter mode id for one of Loom's launch postures. `None` means
    /// the harness has no equivalent and the launch must not silently fall
    /// back to a different permission posture. A harness's own advertised id
    /// maps to itself: a live session persists the adapter id, and a respawn
    /// replays it back through this mapping.
    fn acp_mode_for(&self, mode: &str) -> Option<&'static str>;

    /// The Loom postures this harness can honour, for the picker and for
    /// profile validation. Every mode here has an [`Self::acp_mode_for`]
    /// mapping, so choosing one cannot silently leave the harness on a
    /// different posture.
    fn acp_modes(&self) -> Vec<&'static str> {
        PICKER_POSTURES
            .into_iter()
            .filter(|mode| self.acp_mode_for(mode).is_some())
            .collect()
    }

    /// Whether a mode id is an explicit no-prompt posture for this harness.
    fn auto_approves(&self, mode: &str) -> bool {
        auto_approves_permissions(mode)
    }

    /// The Loom posture a stored adapter mode id stands for. An id with no
    /// Loom equivalent has no answer, and the caller must not silently
    /// substitute one.
    fn loom_mode_for(&self, adapter_mode: &str) -> Option<&'static str> {
        let mode = adapter_mode.trim();
        LOOM_POSTURES
            .into_iter()
            .find(|posture| *posture == mode || self.acp_mode_for(posture) == Some(mode))
    }

    /// The pause between the catalogue probe's per-model option fetches.
    fn probe_pace(&self) -> Duration {
        Duration::ZERO
    }

    /// Whether a fresh ACP launch with no goal seeds the primer positionally —
    /// the adapters with no `appendSystemPrompt` analogue.
    fn seeds_primer_as_goal(&self) -> bool {
        false
    }

    /// Whether a weaver-prelude launch installs the SessionStart hook bundle.
    fn installs_prelude_hooks(&self) -> bool {
        false
    }

    /// The `_meta` sent with `session/new`/`session/load`, if the adapter
    /// takes one.
    fn acp_meta(&self, contract: &AcpContract<'_>) -> Option<Value> {
        let _ = contract;
        None
    }

    /// Adapter-contract environment for the launch. Returns additional working
    /// directories the session needs.
    fn prepare_acp_env(
        &self,
        env: &mut Vec<(String, String)>,
        contract: &AcpContract<'_>,
    ) -> anyhow::Result<Vec<String>> {
        let _ = (env, contract);
        Ok(Vec::new())
    }

    /// Whether the adapter boots in its mode itself (via the launch
    /// environment) rather than accepting a post-setup `session/set_mode`.
    fn boots_in_acp_mode(&self) -> bool {
        false
    }
}

/// The harness for a builtin kind. Exhaustive, so a new `BuiltinAgentKind`
/// variant forces a registration here.
pub fn builtin_for(kind: BuiltinAgentKind) -> &'static dyn BuiltinHarness {
    match kind {
        BuiltinAgentKind::Claude => &claude::HARNESS,
        BuiltinAgentKind::Codex => &codex::HARNESS,
        BuiltinAgentKind::OpenCode => &opencode::HARNESS,
        BuiltinAgentKind::CursorAgent => &cursor::HARNESS,
    }
}

/// The builtin harness `id` names, if any.
pub fn builtin_for_id(id: &str) -> Option<&'static dyn BuiltinHarness> {
    BuiltinAgentKind::parse(id).map(builtin_for)
}

/// Every builtin harness, in picker order.
pub fn all_builtins() -> [&'static dyn BuiltinHarness; 4] {
    [
        &claude::HARNESS,
        &codex::HARNESS,
        &opencode::HARNESS,
        &cursor::HARNESS,
    ]
}

/// The harness that reads credential `name`, if any.
pub fn harness_of_credential(name: &str) -> Option<&'static dyn BuiltinHarness> {
    all_builtins()
        .into_iter()
        .find(|harness| harness.credential_names().contains(&name))
}

/// The adapter mode id a builtin maps a Loom posture onto.
pub fn acp_mode_for(kind: BuiltinAgentKind, mode: &str) -> Option<&'static str> {
    builtin_for(kind).acp_mode_for(mode)
}

/// The Loom postures a builtin can honour.
pub fn acp_modes(kind: BuiltinAgentKind) -> Vec<&'static str> {
    builtin_for(kind).acp_modes()
}

/// The Loom posture a builtin's stored adapter mode id stands for.
pub fn loom_mode_for(kind: BuiltinAgentKind, mode: &str) -> Option<&'static str> {
    builtin_for(kind).loom_mode_for(mode)
}

/// Whether an ACP mode id asks for one-shot auto-approval, for a known
/// harness. Cursor names its *ordinary* mode `agent` too, so the bare id
/// means something different there; Opencode prompts per its own permission
/// config in both of its modes.
pub fn auto_approves_permissions_for(kind: Option<BuiltinAgentKind>, mode: &str) -> bool {
    match kind {
        Some(kind) => builtin_for(kind).auto_approves(mode),
        None => auto_approves_permissions(mode),
    }
}

/// The posture check for a harness Loom cannot name (an operator's custom
/// adapter): the explicit no-prompt mode ids of the builtins that have them.
pub fn auto_approves_permissions(mode: &str) -> bool {
    matches!(
        mode.trim(),
        "bypassPermissions" | "agent-full-access" | codex::CODEX_AGENT_MODE
    )
}

// -- shared harness helpers -----------------------------------------------

/// A non-blank model selection, trimmed.
pub(crate) fn model_flag(model: &str) -> Option<&str> {
    let model = model.trim();
    (!model.is_empty()).then_some(model)
}

/// A non-blank effort selection, trimmed.
pub(crate) fn effort_flag(effort: &str) -> Option<&str> {
    let effort = effort.trim();
    (!effort.is_empty()).then_some(effort)
}

pub(crate) fn join_args(args: impl IntoIterator<Item = Option<String>>) -> String {
    args.into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// A leading space before `args`, or nothing when blank.
pub(crate) fn spaced(args: String) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" {args}")
    }
}

/// Append `(key, value)` unless `key` is already present (an operator override
/// via extra_env wins over the adapter-contract default).
pub(crate) fn push_env_default(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !env.iter().any(|(k, _)| k == key) {
        env.push((key.to_string(), value.to_string()));
    }
}

/// The default command for an npm-distributed ACP adapter: the installed bin
/// when present (the deploy pins exact versions onto PATH), else `npx`
/// fetching the package at launch (the dev-machine path).
pub(crate) fn npm_adapter_cmd(bin: &str, package: &str) -> String {
    format!("command -v {bin} >/dev/null 2>&1 && exec {bin}; exec npx --yes {package}")
}

/// An operator override for an ACP adapter command: an env var wins, then a
/// config setting.
pub(crate) async fn acp_cmd_override(db: &Db, env_var: &str, setting: &str) -> Option<String> {
    if let Ok(cmd) = std::env::var(env_var) {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            return Some(cmd.to_string());
        }
    }
    weaver_core::config::get(db, setting)
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether `bin` resolves to an executable file on `PATH`.
///
/// Reports presence, not health: a binary that exists but misbehaves still
/// counts as available. On Windows every `PATHEXT` suffix is tried, since an
/// npm-installed CLI ships as `bin.cmd`/`bin.ps1` and never `bin.exe`;
/// elsewhere the file must be a regular file with an execute bit. The
/// filesystem walk runs on a blocking thread so a slow `PATH` entry cannot
/// stall the async runtime.
pub(crate) async fn binary_on_path(bin: &str) -> bool {
    let bin = bin.to_string();
    tokio::task::spawn_blocking(move || {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        let names = candidate_names(&bin);
        std::env::split_paths(&path)
            .any(|dir| names.iter().any(|name| is_executable_file(&dir.join(name))))
    })
    .await
    .unwrap_or(false)
}

#[cfg(windows)]
fn candidate_names(bin: &str) -> Vec<String> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut names = vec![bin.to_string()];
    for ext in exts.split(';').filter(|ext| !ext.is_empty()) {
        names.push(format!("{bin}{}", ext.to_ascii_lowercase()));
    }
    names
}

#[cfg(not(windows))]
fn candidate_names(bin: &str) -> [String; 1] {
    [bin.to_string()]
}

#[cfg(windows)]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(not(windows))]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, source).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_file_rejects_directories_and_unset_execute_bits() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!is_executable_file(&dir.path().join("missing")));

        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        assert!(!is_executable_file(&subdir));

        let plain = dir.path().join("plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(!is_executable_file(&plain));

        let script = dir.path().join("script");
        write_executable(&script, "#!/bin/sh\n");
        assert!(is_executable_file(&script));
    }

    #[test]
    fn push_env_default_defers_to_an_existing_key() {
        let mut env = vec![(
            "CODEX_CONFIG".to_string(),
            "{\"model\":\"mine\"}".to_string(),
        )];
        push_env_default(&mut env, "CODEX_CONFIG", "{\"model\":\"ours\"}");
        push_env_default(&mut env, "INITIAL_AGENT_MODE", "agent");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].1, "{\"model\":\"mine\"}");
        assert_eq!(
            env[1],
            ("INITIAL_AGENT_MODE".to_string(), "agent".to_string())
        );
    }

    #[test]
    fn every_harness_round_trips_its_id() {
        for harness in all_builtins() {
            assert_eq!(harness.kind().as_str(), harness.metadata().kind);
        }
    }

    #[test]
    fn every_credential_names_one_harness() {
        for harness in all_builtins() {
            for name in harness.credential_names() {
                assert_eq!(
                    harness_of_credential(name).map(|h| h.kind()),
                    Some(harness.kind())
                );
            }
        }
    }

    /// A live session persists the adapter's mode id in `current_mode` and a
    /// respawn replays it through this mapping, so every id an adapter
    /// advertises must map to itself — not just the harnesses whose ids happen
    /// to match Loom's vocabulary.
    #[test]
    fn a_harness_replays_its_own_advertised_mode_ids() {
        let native: &[(&dyn BuiltinHarness, &[&str])] = &[
            (
                &claude::HARNESS,
                &[
                    "auto",
                    "default",
                    "acceptEdits",
                    "plan",
                    "bypassPermissions",
                ],
            ),
            (
                &codex::HARNESS,
                &["read-only", "agent", "agent-full-access"],
            ),
            (&opencode::HARNESS, &["build", "plan"]),
            (&cursor::HARNESS, &["agent", "plan", "ask"]),
        ];
        for (harness, ids) in native {
            for id in *ids {
                assert_eq!(
                    harness.acp_mode_for(id),
                    Some(*id),
                    "{} must replay its own '{id}'",
                    harness.kind().as_str()
                );
            }
        }
    }

    #[test]
    fn a_posture_a_harness_cannot_express_is_still_rejected() {
        for harness in [&opencode::HARNESS as &dyn BuiltinHarness, &cursor::HARNESS] {
            assert_eq!(harness.acp_mode_for("bypassPermissions"), None);
            assert_eq!(harness.acp_mode_for("acceptEdits"), None);
            assert_eq!(harness.acp_mode_for("not-a-mode"), None);
        }
    }

    #[test]
    fn cursor_and_opencode_do_not_auto_approve_their_ordinary_modes() {
        assert!(!cursor::HARNESS.auto_approves("agent"));
        assert!(!opencode::HARNESS.auto_approves("build"));
        assert!(cursor::HARNESS.auto_approves("bypassPermissions"));
        assert!(!opencode::HARNESS.auto_approves("bypassPermissions"));
    }
}
