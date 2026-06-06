//! Agent-facing helpers that are pure (no tmux, no process spawning): the
//! Claude Code hook config and the SessionStart primer (a WEAVER.md).

use serde_json::{json, Value};

/// Claude Code hook config that reports session status to weaver.
///
/// Hooks shell out to `weaver hook --event <name>`. The CLI itself resolves the
/// current branch (from `$WEAVER_BRANCH` or cwd) and writes an `events` row;
/// the orchestrator picks it up on its monitor tick. No daemon required.
pub fn hooks_json(weaver_bin: &str) -> Value {
    let hook = |event: &str| {
        json!([{
            "hooks": [{
                "type": "command",
                "command": format!("{weaver_bin} hook --event {event}")
            }]
        }])
    };
    json!({
        "hooks": {
            "SessionStart": hook("session-start"),
            "UserPromptSubmit": hook("working"),
            "Notification": hook("waiting"),
            "Stop": hook("idle"),
        }
    })
}

/// The builtin WEAVER.md — how an agent works inside a weaver session — kept as
/// a standalone markdown doc and catted in at build time so `weaver hook` stays
/// self-contained wherever it runs. A repo may ship its own `WEAVER.md` to
/// override this; see [`session_primer`].
const BUILTIN_WEAVER_MD: &str = include_str!("../WEAVER.md");

/// The builtin WEAVER.md, used when the repo doesn't ship its own.
pub fn builtin_weaver_md() -> &'static str {
    BUILTIN_WEAVER_MD
}

/// Context injected at SessionStart (via the `session-start` weaver hook): a
/// WEAVER.md telling the agent it is in a weaver session and how it is expected
/// to behave. `weaver_md` is the repo's own WEAVER.md when present, else
/// [`builtin_weaver_md`].
pub fn session_primer(weaver_md: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": weaver_md,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_point_at_the_weaver_binary() {
        let hooks = hooks_json("/usr/bin/weaver");
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(stop, "/usr/bin/weaver hook --event idle");
    }

    #[test]
    fn session_start_hook_uses_a_distinct_event() {
        let hooks = hooks_json("/usr/bin/weaver");
        let cmd = hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, "/usr/bin/weaver hook --event session-start");
    }

    #[test]
    fn session_primer_wraps_the_builtin_weaver_md() {
        let v: Value = serde_json::from_str(&session_primer(builtin_weaver_md())).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert!(v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("weaver set-status"));
    }

    #[test]
    fn session_primer_passes_a_repo_override_through_verbatim() {
        let custom = "# Our team's weaver workflow\nrun `make ci` before any PR.";
        let v: Value = serde_json::from_str(&session_primer(custom)).unwrap();
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], custom);
    }
}
