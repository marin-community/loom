//! The committed per-repo `.weaver/config.toml`.
//!
//! A repo ships this file to configure the weaver sessions launched against it: a
//! `[setup]` bootstrap (install deps, prime caches), `[env]` defaults exported
//! into the agent terminal, and optional `[agent]` defaults (which
//! agent/model/effort a new session uses when the launch request does not pin
//! them). It is the file-based sibling of the operator's database-backed
//! settings: repo-file values resolve *over* the builtin defaults, exactly as a
//! repo's own `WEAVER.md` overrides the builtin session primer.
//!
//! This module is a pure loader/parser — it reads the file and exposes the parsed
//! shape, nothing more. Layering the `[env]` over the operator env and *running*
//! the `[setup]` script live in `loom` (the service that launches sessions and
//! holds the per-repo allowlist); `weaver-core` only knows how to read the file.
//! The loader is deliberately lenient about unknown keys so a repo can add
//! sections a newer loom understands without breaking an older one.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// The config file's path within a repo checkout.
pub const CONFIG_REL_PATH: &str = ".weaver/config.toml";

/// A parsed `.weaver/config.toml`. Every section is optional; an absent file
/// yields [`RepoConfig::default`] (all empty).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepoConfig {
    /// The bootstrap to run in the worktree before the agent starts.
    #[serde(default)]
    pub setup: Setup,
    /// Environment defaults exported into the session terminal. Held in a
    /// `BTreeMap` so the export order is deterministic (sorted by name), like the
    /// operator's `agent_env`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Default agent/model/effort for sessions launched against this repo.
    #[serde(default)]
    pub agent: AgentDefaults,
}

/// The `[setup]` section. Provide either a `script` (one shell snippet) or
/// `commands` (a list run in order); if both are present, `script` wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Setup {
    /// A shell snippet run as a single script.
    #[serde(default)]
    pub script: Option<String>,
    /// Commands run in order — joined into one fail-fast script. Ignored when
    /// `script` is set.
    #[serde(default)]
    pub commands: Vec<String>,
}

impl Setup {
    /// The shell snippet to run, or `None` when no setup is configured. A
    /// non-blank `script` is returned verbatim; otherwise the non-blank
    /// `commands` are joined with newlines (the caller runs the result fail-fast,
    /// so a failing line aborts the rest). An all-blank section yields `None`.
    pub fn script(&self) -> Option<String> {
        if let Some(script) = self.script.as_ref() {
            if !script.trim().is_empty() {
                return Some(script.clone());
            }
        }
        let commands: Vec<&str> = self
            .commands
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if commands.is_empty() {
            None
        } else {
            Some(commands.join("\n"))
        }
    }

    /// Whether this section configures nothing to run.
    pub fn is_empty(&self) -> bool {
        self.script().is_none()
    }
}

/// The `[agent]` section: the defaults a new session falls back to when the
/// launch request does not pin them. Each is the same selector the create API
/// accepts; an empty/absent value defers to the operator's global default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AgentDefaults {
    /// Default agent kind (`claude`, `codex`, `shell`, …).
    #[serde(default)]
    pub default: Option<String>,
    /// Default model selector for the chosen agent.
    #[serde(default)]
    pub model: Option<String>,
    /// Default reasoning effort for the chosen agent.
    #[serde(default)]
    pub effort: Option<String>,
}

/// Parse a `.weaver/config.toml` from its text.
pub fn parse(text: &str) -> Result<RepoConfig> {
    toml::from_str(text).context("parsing .weaver/config.toml")
}

/// Load the `.weaver/config.toml` committed in `dir`'s checkout. A missing file
/// is not an error — it yields the default (empty) config; only a present but
/// malformed file errors, so a typo surfaces rather than being silently ignored.
pub fn load(dir: &Path) -> Result<RepoConfig> {
    let path = dir.join(CONFIG_REL_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RepoConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Resolve the config from the first of `dirs` that ships a
/// `.weaver/config.toml`, mirroring `WEAVER.md` resolution (the worktree first,
/// then the primary checkout). A present-but-malformed file still errors; a `dir`
/// without the file is skipped. No `dir` having it yields the default config.
pub fn resolve(dirs: &[&Path]) -> Result<RepoConfig> {
    for dir in dirs {
        if dir.join(CONFIG_REL_PATH).exists() {
            return load(dir);
        }
    }
    Ok(RepoConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_sections() {
        let cfg = parse(
            r#"
            [setup]
            script = "uv sync"

            [env]
            DATABASE_URL = "postgres://localhost/dev"
            RUST_LOG = "debug"

            [agent]
            default = "claude"
            model = "opus"
            effort = "high"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.setup.script().as_deref(), Some("uv sync"));
        assert_eq!(
            cfg.env.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost/dev")
        );
        // BTreeMap keeps a deterministic, sorted order.
        let names: Vec<&str> = cfg.env.keys().map(String::as_str).collect();
        assert_eq!(names, ["DATABASE_URL", "RUST_LOG"]);
        assert_eq!(cfg.agent.default.as_deref(), Some("claude"));
        assert_eq!(cfg.agent.model.as_deref(), Some("opus"));
        assert_eq!(cfg.agent.effort.as_deref(), Some("high"));
    }

    #[test]
    fn empty_text_is_the_default_config() {
        assert_eq!(parse("").unwrap(), RepoConfig::default());
    }

    #[test]
    fn unknown_sections_are_ignored_for_forward_compat() {
        // A repo may ship a section a newer loom understands; an older loader
        // must not choke on it.
        let cfg = parse(
            r#"
            [setup]
            script = "make deps"

            [deploy]
            target = "fly"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.setup.script().as_deref(), Some("make deps"));
    }

    #[test]
    fn commands_join_into_one_fail_fast_script() {
        let cfg = parse(
            r#"
            [setup]
            commands = ["npm ci", "npm run build", "  "]
            "#,
        )
        .unwrap();
        // Blank entries are dropped; the rest join newline-separated.
        assert_eq!(cfg.setup.script().as_deref(), Some("npm ci\nnpm run build"));
    }

    #[test]
    fn script_wins_over_commands_when_both_present() {
        let cfg = parse(
            r#"
            [setup]
            script = "./bootstrap.sh"
            commands = ["ignored"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.setup.script().as_deref(), Some("./bootstrap.sh"));
    }

    #[test]
    fn blank_setup_is_empty() {
        let cfg = parse(
            r#"
            [setup]
            script = "   "
            "#,
        )
        .unwrap();
        assert!(cfg.setup.is_empty());
        assert_eq!(cfg.setup.script(), None);
    }

    #[test]
    fn malformed_toml_errors() {
        assert!(parse("this is = = not toml").is_err());
    }

    #[test]
    fn load_missing_file_is_default_resolve_prefers_first_present() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Neither dir has the file → default.
        assert_eq!(load(a.path()).unwrap(), RepoConfig::default());
        assert_eq!(
            resolve(&[a.path(), b.path()]).unwrap(),
            RepoConfig::default()
        );

        // Write a config into `b` only; resolve([a, b]) skips a and picks b.
        std::fs::create_dir_all(b.path().join(".weaver")).unwrap();
        std::fs::write(
            b.path().join(CONFIG_REL_PATH),
            "[setup]\nscript = \"echo b\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve(&[a.path(), b.path()])
                .unwrap()
                .setup
                .script()
                .as_deref(),
            Some("echo b")
        );

        // A config in `a` takes precedence (worktree-first), like WEAVER.md.
        std::fs::create_dir_all(a.path().join(".weaver")).unwrap();
        std::fs::write(
            a.path().join(CONFIG_REL_PATH),
            "[setup]\nscript = \"echo a\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve(&[a.path(), b.path()])
                .unwrap()
                .setup
                .script()
                .as_deref(),
            Some("echo a")
        );
    }

    #[test]
    fn load_malformed_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".weaver")).unwrap();
        std::fs::write(dir.path().join(CONFIG_REL_PATH), "= bad").unwrap();
        assert!(load(dir.path()).is_err());
    }
}
