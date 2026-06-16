//! The **builtin overlooker programs** — the stock programs that ship inside
//! the loom binary and that an overlooker's `program` field names as
//! `builtin:<name>`.
//!
//! Every builtin is a **script**: a real Python file under
//! `crates/loom/overlookers/`, embedded here with `include_str!` and run by
//! the engine's script executor ([`crate::overlooker`]) exactly like a user's
//! custom program file. Living in the repo makes each one diffable,
//! reviewable, and the working example of the program contract; the panel
//! shows the source read-only. There is deliberately no privileged in-Rust
//! program shape — everything a builtin does, it does through the loom REST
//! API a custom program also sees.
//!
//! `GET /api/overlookers/programs` serves this table (as [`ProgramView`]s) so
//! the panel and the `loom overlooker programs` CLI list one source of truth.

use serde_json::{json, Value};
use weaver_api::ProgramView;

/// The `weaver_loom` Python module — the API layer over the loom REST API that
/// overlooker programs import. Vendored into the binary so the engine can
/// place it on every script's `PYTHONPATH` (no install step); the source of
/// truth (and the installable package) is `python/weaver-loom/`.
pub const PYTHON_MODULE: &str =
    include_str!("../../../python/weaver-loom/src/weaver_loom/__init__.py");

/// One stock program: its identity, its embedded source, and the suggested
/// starting config a create form prefills. The JSON-bearing defaults are
/// stored as text so the table stays a flat `const`.
pub struct BuiltinProgram {
    /// The short name; the program reference is `builtin:<name>`.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The embedded Python source.
    pub source: &'static str,
    pub default_trigger: &'static str,
    pub default_scope: &'static str,
    pub default_params: &'static str,
    pub default_capabilities: &'static [&'static str],
}

/// Every builtin program, in the order the panel lists them.
pub const BUILTINS: &[BuiltinProgram] = &[
    BuiltinProgram {
        name: "status",
        title: "Status check",
        description: "When a session goes idle (the agent's finished-turn hook), \
                      ask the judge model (the daemon's one-shot agent) for the \
                      set of attention tags it warrants and reconcile the watch's \
                      own marks to that set. Names the kind of attention (review, \
                      question, stuck, …) rather than a generic mark, and when it \
                      finds a genuine need it replaces the soothing `idle` mark \
                      with that real status. With no judge model available it \
                      no-ops, never mirroring the agent's own attention tag.",
        source: include_str!("../overlookers/status.py"),
        default_trigger: r#"{"on":["session.idle"]}"#,
        default_scope: "{}",
        default_params: "{}",
        default_capabilities: &["observe", "mark"],
    },
    BuiltinProgram {
        name: "pr-label",
        title: "PR labeller",
        description: "Flag sessions whose open pull request lacks the loom \
                      label (params.label, default 'weaver'), so PRs born from \
                      sessions are identifiable on GitHub. Read-only: it \
                      reports would-label actions.",
        source: include_str!("../overlookers/pr_label.py"),
        default_trigger: r#"{"on":["pr.opened"]}"#,
        default_scope: "{}",
        default_params: r#"{"label":"weaver"}"#,
        default_capabilities: &["observe"],
    },
    BuiltinProgram {
        name: "archive-merged",
        title: "Archive merged",
        description: "Flag live sessions whose pull request has merged — \
                      integrated work whose session is ready to archive. \
                      Read-only: it reports would-archive actions (the \
                      github.archive_on_merge setting still performs the \
                      archive).",
        source: include_str!("../overlookers/archive_merged.py"),
        default_trigger: r#"{"on":["pr.merged"]}"#,
        default_scope: "{}",
        default_params: "{}",
        default_capabilities: &["observe"],
    },
];

impl BuiltinProgram {
    /// The reference an overlooker's `program` field uses: `builtin:<name>`.
    pub fn program(&self) -> String {
        format!("builtin:{}", self.name)
    }

    /// The wire view: defaults parsed into structured JSON, source attached.
    pub fn view(&self) -> ProgramView {
        let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({}));
        ProgramView {
            program: self.program(),
            title: self.title.to_string(),
            description: self.description.to_string(),
            source: self.source.to_string(),
            defaults: json!({
                "trigger": parse(self.default_trigger),
                "scope": parse(self.default_scope),
                "params": parse(self.default_params),
                "capabilities": self.default_capabilities,
            }),
        }
    }
}

/// Resolve a `builtin:<name>` program reference against the registry. Any
/// other shape (a file path, an unknown builtin) is `None`.
pub fn find(program: &str) -> Option<&'static BuiltinProgram> {
    let name = program.strip_prefix("builtin:")?;
    BUILTINS.iter().find(|b| b.name == name)
}

/// Whether `python3` is on PATH — whether script programs can run here. The
/// engine itself degrades per-round (a missing interpreter errors that round
/// with a clear message); this probe is for the call sites that want to know
/// up front, e.g. tests that skip rather than fail without an interpreter.
pub fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_resolves_builtin_refs_only() {
        assert_eq!(find("builtin:status").unwrap().name, "status");
        assert_eq!(find("builtin:pr-label").unwrap().name, "pr-label");
        assert!(find("builtin:nope").is_none());
        assert!(find("/abs/path.py").is_none());
        assert!(find("status").is_none());
    }

    #[test]
    fn registry_is_well_formed() {
        for b in BUILTINS {
            // The default consts must be real JSON objects — parsed strictly
            // here, so a typo'd const fails the test rather than silently
            // becoming the `{}` fallback `view()` applies.
            for (field, raw) in [
                ("trigger", b.default_trigger),
                ("scope", b.default_scope),
                ("params", b.default_params),
            ] {
                let parsed: Value = serde_json::from_str(raw)
                    .unwrap_or_else(|e| panic!("{}: default {field} is not JSON: {e}", b.name));
                assert!(parsed.is_object(), "{}: default {field}", b.name);
            }
            for cap in b.default_capabilities {
                assert!(
                    weaver_core::overlooker::CAPABILITIES.contains(cap),
                    "{}: unknown capability {cap}",
                    b.name
                );
            }
            // The wire view: every builtin serves its embedded source.
            let v = b.view();
            assert!(v.program.starts_with("builtin:"));
            assert!(!v.source.is_empty(), "{}: source is embedded", b.name);
        }
    }

    /// Every embedded script must at least be valid Python — `py_compile` is
    /// the cheap structural gate. Skips when `python3` is absent (the same
    /// graceful degradation the engine applies at run time).
    #[test]
    fn embedded_scripts_compile() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let scripts = BUILTINS
            .iter()
            .map(|b| (b.name, b.source))
            .chain([("weaver_loom", PYTHON_MODULE)]);
        for (name, source) in scripts {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{name}.py"));
            std::fs::write(&path, source).unwrap();
            let out = std::process::Command::new("python3")
                .args(["-m", "py_compile"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name} does not compile: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
