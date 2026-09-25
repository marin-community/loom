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
    /// Only `SessionStart` (primer + compaction re-orientation). An ACP
    /// session's turn boundaries come from the protocol itself, so loom drives
    /// status/idle from the ACP turn edges instead (see `loom::acp` /
    /// `loom::monitor`).
    Acp,
}

/// Claude Code hook config that reports session status to Loom.
///
/// Hooks shell out to `loom hook --event <name>`. The CLI itself resolves the
/// current branch (from `$WEAVER_BRANCH` or cwd) and writes an `events` row;
/// the orchestrator picks it up on its monitor tick. No daemon required.
///
/// `mode` selects the bundle — see [`HookMode`].
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

/// Compact builtin orientation. Full command reference belongs to `loom help`.
const BUILTIN_WEAVER_MD: &str = r#"# Loom session

You are working in a detached Loom session. Your opening task is the goal.

- Discover commands: `loom help`, then `loom help channels` or `loom sessions launch --help`.
- Recover context after interruption or compaction: `loom summary`.
- Check access: `loom permissions show`. For another GitHub repository, run `loom permissions request github-repository owner/repo --reason "..."`; the request reaches a person in the web UI. Do not ask them to run a shell command.
- Report progress or a need for help: `loom status set --tag ok --message "tests running"` (use `attention` or `blocked` when a person must act).
- Read and send durable messages: `loom channels read`; `loom channels send "question or update"`.
- Save a deliverable: `loom artifacts write plan plan.md`. Delegate: `loom sessions launch "implement the parser"`.
- Read a child's result: `loom channels read --channel <child-id> --kinds result`. A child's typed result also notifies its parent channel.
- Finish delegated work: `loom channels send --kind result "<outcome or PR>"`.

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
