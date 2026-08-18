//! Agent-facing helpers that are pure (no terminal management, no process spawning): the
//! Claude Code hook config and the compact SessionStart primer.

use serde_json::{json, Map, Value};

/// Which hook bundle [`hooks_json`] installs, chosen by the session's execution
/// backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookMode {
    /// The full lifecycle bundle for a terminal (PTY) session: `SessionStart`
    /// (primer) plus the work-cycle hooks (`UserPromptSubmit`/`Notification`/
    /// `Stop`) that drive working/waiting/idle.
    Terminal,
    /// Only `SessionStart` (primer + compaction re-orientation) — for an ACP
    /// session, whose turn boundaries come from the protocol itself, so the
    /// work-cycle hooks are redundant and are dropped (loom drives status/idle
    /// from the ACP turn edges instead; see `loom::acp` / `loom::monitor`).
    Acp,
}

/// Claude Code hook config that reports session status to Loom.
///
/// Hooks shell out to `loom hook --event <name>`. The CLI itself resolves the
/// current branch (from `$WEAVER_BRANCH` or cwd) and writes an `events` row;
/// the orchestrator picks it up on its monitor tick. No daemon required.
///
/// `mode` selects the bundle: a terminal session installs the full set;
/// an ACP session installs only `SessionStart`, since its working/idle edges
/// are the protocol's turn boundaries, not the work-cycle hooks.
pub fn hooks_json(loom_bin: &str, mode: HookMode) -> Value {
    let hook = |event: &str| {
        json!([{
            "hooks": [{
                "type": "command",
                "command": format!("{loom_bin} hook --event {event}")
            }]
        }])
    };
    let mut hooks = Map::new();
    hooks.insert("SessionStart".into(), hook("session-start"));
    if mode == HookMode::Terminal {
        hooks.insert("UserPromptSubmit".into(), hook("working"));
        hooks.insert("Notification".into(), hook("waiting"));
        hooks.insert("Stop".into(), hook("idle"));
    }
    json!({ "hooks": hooks })
}

/// Compact builtin orientation. Executable reference belongs to Loom's
/// operation registry and `loom help`, not a separately maintained Markdown
/// catalogue. Repository-specific `WEAVER.md` overrides remain readable during
/// the compatibility window.
const BUILTIN_WEAVER_MD: &str = r#"# Loom session

You are working in a detached Loom session. Your opening task is the goal.

- Run `loom summary` to recover durable context after interruption or compaction.
- Run `loom help` to discover resource groups and `loom <group> --help` for commands.
- Run `loom permissions show` to inspect effective access; request another GitHub repository with `loom permissions request github-repository owner/repo --reason "..."`.
- Keep `loom status set --tag <ok|attention|blocked> --message "..."` honest. `attention` and `blocked` mean a person must act.
- Use `loom channels read` and `loom channels send` for durable communication.
- Finish delegated work with `loom channels send --kind result "<outcome or PR>"`.

Repository-specific engineering and landing rules live in `AGENTS.md`.
"#;

/// The builtin WEAVER.md, used when the repo doesn't ship its own.
pub fn builtin_weaver_md() -> &'static str {
    BUILTIN_WEAVER_MD
}

/// Wrap `context` as the JSON a SessionStart hook prints to inject it into the
/// agent's context (`hookSpecificOutput.additionalContext`). On a genuine
/// start/resume/clear this carries the full WEAVER.md primer (the repo's own
/// when present, else [`builtin_weaver_md`]); after a compaction the Loom hook
/// passes a concise re-orientation instead, so the agent isn't re-fed the whole
/// guide every time its context is summarized.
pub fn session_primer(context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_point_at_the_loom_binary() {
        let hooks = hooks_json("/usr/bin/loom", HookMode::Terminal);
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(stop, "/usr/bin/loom hook --event idle");
    }

    #[test]
    fn session_start_hook_uses_a_distinct_event() {
        let hooks = hooks_json("/usr/bin/loom", HookMode::Terminal);
        let cmd = hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, "/usr/bin/loom hook --event session-start");
    }

    #[test]
    fn acp_mode_installs_only_the_session_start_hook() {
        // An ACP session's turn boundaries are the protocol's, so only the
        // primer hook is installed — the work-cycle hooks are dropped.
        let hooks = hooks_json("/usr/bin/loom", HookMode::Acp);
        let obj = hooks["hooks"].as_object().unwrap();
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["SessionStart"],
            "only SessionStart is installed for acp: {hooks}"
        );
        assert!(obj.get("UserPromptSubmit").is_none());
        assert!(obj.get("Stop").is_none());
        assert!(obj.get("Notification").is_none());
    }

    #[test]
    fn session_primer_wraps_the_builtin_weaver_md() {
        let v: Value = serde_json::from_str(&session_primer(builtin_weaver_md())).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert!(v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("loom status"));
    }

    #[test]
    fn session_primer_passes_a_repo_override_through_verbatim() {
        let custom = "# Our team's weaver workflow\nrun `make ci` before any PR.";
        let v: Value = serde_json::from_str(&session_primer(custom)).unwrap();
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], custom);
    }
}
