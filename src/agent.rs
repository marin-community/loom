//! Launching coding agents into tmux panes, installing status hooks, and
//! running headless summary passes.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// The command run inside the tmux pane for a given agent kind.
///
/// `claude` launches the interactive TUI seeded with the goal; `shell`/`none`
/// just drop into a shell; anything else is treated as a custom command that
/// receives the goal file's path as its single argument.
fn inner_command(agent_kind: &str, goal_file: &Path) -> String {
    let goal_file = goal_file.display();
    match agent_kind {
        "shell" | "none" => String::new(),
        "claude" => format!("claude \"$(cat '{goal_file}')\""),
        other => format!("{other} '{goal_file}'"),
    }
}

/// Full `sh -c` script for the workspace's tmux session: exports env, runs the
/// agent, then drops to an interactive shell so the pane survives agent exit.
pub fn launch_script(agent_kind: &str, goal_file: &Path, env: &[(&str, &str)]) -> String {
    let mut script = String::new();
    for (k, v) in env {
        script.push_str(&format!("export {k}='{v}'; "));
    }
    let inner = inner_command(agent_kind, goal_file);
    if !inner.is_empty() {
        script.push_str(&inner);
        script.push_str("; ");
    }
    script.push_str("exec \"${SHELL:-/bin/sh}\"");
    script
}

/// Claude Code hook config that reports workspace status back to weaver.
pub fn hooks_json(weaver_bin: &str, workspace_id: &str) -> Value {
    let hook = |event: &str| {
        json!([{
            "hooks": [{
                "type": "command",
                "command": format!("{weaver_bin} hook --workspace {workspace_id} --event {event}")
            }]
        }])
    };
    json!({
        "hooks": {
            "SessionStart": hook("working"),
            "UserPromptSubmit": hook("working"),
            "Notification": hook("waiting"),
            "Stop": hook("idle"),
        }
    })
}

/// Write (merging into any existing file) `.claude/settings.local.json` so the
/// agent reports status to weaver via hooks.
pub async fn install_hooks(work_dir: &Path, weaver_bin: &str, workspace_id: &str) -> Result<()> {
    let dir = work_dir.join(".claude");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("settings.local.json");
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    let hooks = hooks_json(weaver_bin, workspace_id);
    root["hooks"] = hooks["hooks"].clone();
    tokio::fs::write(&path, serde_json::to_string_pretty(&root)?).await?;
    Ok(())
}

const SUMMARY_PROMPT: &str = "You are summarizing the current state of a coding workspace for a \
status dashboard. The git diff of all work-in-progress is provided on stdin. Reply with 2-4 \
plain sentences describing what has changed so far and the apparent state of the work. No \
preamble, no markdown, no bullet points.";

const MAX_DIFF_CHARS: usize = 80_000;

/// Run a headless `claude -p` pass over a diff and return its summary text.
pub async fn summarize(work_dir: &Path, diff: &str) -> Result<String> {
    let mut diff = diff.to_string();
    if diff.len() > MAX_DIFF_CHARS {
        diff.truncate(MAX_DIFF_CHARS);
        diff.push_str("\n...[diff truncated]");
    }
    let mut child = Command::new("claude")
        .args(["-p", SUMMARY_PROMPT])
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn claude (is it installed and on PATH?)")?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes()).await;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait_with_output())
        .await
        .context("claude summary timed out")??;
    if !out.status.success() {
        anyhow::bail!(
            "claude summary failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shell_script_just_execs_a_shell() {
        let script = launch_script("shell", Path::new("/x/goal.txt"), &[]);
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn claude_script_exports_env_and_runs_claude() {
        let script = launch_script(
            "claude",
            Path::new("/x/goal.txt"),
            &[("WEAVER_API", "http://h:1")],
        );
        assert!(script.contains("export WEAVER_API='http://h:1'; "));
        assert!(script.contains("claude \"$(cat '/x/goal.txt')\"; "));
        assert!(script.ends_with("exec \"${SHELL:-/bin/sh}\""));
    }

    #[test]
    fn hooks_point_at_the_weaver_binary() {
        let hooks = hooks_json("/usr/bin/weaver", "abc12345");
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(stop, "/usr/bin/weaver hook --workspace abc12345 --event idle");
    }
}
