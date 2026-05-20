//! Launching coding agents into tmux panes, installing status hooks, and
//! running headless summary passes.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::tmux;

/// Whether a workspace's tmux session is being created for the first time or
/// recreated to recover ("adopt") an existing worktree whose session died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// First launch: seed the agent with the workspace goal.
    Fresh,
    /// Re-launch into an existing worktree: resume rather than restart.
    Adopt,
}

/// The command run inside the tmux pane for a given agent kind.
///
/// `claude` launches the interactive TUI: in [`LaunchMode::Fresh`] it is seeded
/// with the goal when one was given (otherwise plain `claude`); in
/// [`LaunchMode::Adopt`] it runs `claude --continue` to resume the most recent
/// conversation in the worktree (the goal is ignored — re-seeding it would
/// restart the agent from scratch). `claude_args` (from the `agent.claude_args`
/// setting, e.g. `--model claude-opus-4-7`) is spliced in for both modes.
///
/// `shell`/`none` just drop into a shell. Anything else is treated as a custom
/// command that receives the goal file's path as its single argument; custom
/// agents have no resume concept and ignore `claude_args` — both modes relaunch
/// identically.
fn inner_command(
    agent_kind: &str,
    goal_file: Option<&Path>,
    mode: LaunchMode,
    claude_args: &str,
) -> String {
    // Normalise the extra Claude flags to a leading-space fragment so it can be
    // spliced into the command, or contributes nothing when empty.
    let args = match claude_args.trim() {
        "" => String::new(),
        a => format!(" {a}"),
    };
    match agent_kind {
        "shell" | "none" => String::new(),
        "claude" => match mode {
            LaunchMode::Adopt => format!("claude --continue{args}"),
            LaunchMode::Fresh => match goal_file {
                Some(f) => format!("claude{args} \"$(cat '{}')\"", f.display()),
                None => format!("claude{args}"),
            },
        },
        other => match goal_file {
            Some(f) => format!("{other} '{}'", f.display()),
            None => other.to_string(),
        },
    }
}

/// Full `sh -c` script for the workspace's tmux session: puts the weaver binary
/// on `PATH`, exports env, runs the agent, then drops to an interactive shell so
/// the pane survives agent exit.
pub fn launch_script(
    agent_kind: &str,
    goal_file: Option<&Path>,
    env: &[(&str, &str)],
    weaver_dir: Option<&Path>,
    mode: LaunchMode,
    claude_args: &str,
) -> String {
    let mut script = String::new();
    // Prepend weaver's own directory so the agent can always call `weaver`.
    // Double-quoted so the existing `$PATH` still expands.
    if let Some(dir) = weaver_dir {
        script.push_str(&format!("export PATH=\"{}:$PATH\"; ", dir.display()));
    }
    for (k, v) in env {
        script.push_str(&format!("export {k}='{v}'; "));
    }
    let inner = inner_command(agent_kind, goal_file, mode, claude_args);
    if !inner.is_empty() {
        script.push_str(&inner);
        script.push_str("; ");
    }
    script.push_str("exec \"${SHELL:-/bin/sh}\"");
    script
}

/// Everything [`launch`] needs to bring up a workspace's tmux session. Borrowed
/// fields so both `create_workspace` (pre-insert) and the adopt path (post-load)
/// can build one cheaply.
pub struct LaunchSpec<'a> {
    pub workspace_id: &'a str,
    pub agent_kind: &'a str,
    pub work_dir: &'a Path,
    pub tmux_session: &'a str,
    /// The goal file, when one exists (used only in [`LaunchMode::Fresh`]).
    pub goal_file: Option<&'a Path>,
    /// `host:port` the weaver server is bound to; becomes `WEAVER_API`.
    pub server_addr: &'a str,
    /// Extra arguments for the Claude TUI (the `agent.claude_args` setting).
    /// Ignored for non-claude agents.
    pub claude_args: &'a str,
}

/// Bring up the workspace's tmux session running the agent.
///
/// This is the shared launch sequence used both when creating a workspace and
/// when adopting an orphaned one: it resolves the weaver binary for `PATH`,
/// builds the `WEAVER_API`/`WEAVER_WORKSPACE` env, reinstalls Claude Code hooks
/// for claude-backed workspaces (idempotent), builds the launch script, and
/// starts a detached tmux session.
pub async fn launch(spec: &LaunchSpec<'_>, mode: LaunchMode) -> Result<()> {
    let weaver_exe = std::env::current_exe().ok();
    let weaver_bin = weaver_exe
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "weaver".to_string());
    let weaver_dir = weaver_exe.as_deref().and_then(Path::parent);

    if spec.agent_kind == "claude" {
        // `install_hooks` is idempotent — safe to re-run on adoption.
        install_hooks(spec.work_dir, &weaver_bin, spec.workspace_id)
            .await
            .ok();
    }

    let api_url = format!("http://{}", spec.server_addr);
    let env = [
        ("WEAVER_API", api_url.as_str()),
        ("WEAVER_WORKSPACE", spec.workspace_id),
    ];
    let script = launch_script(
        spec.agent_kind,
        spec.goal_file,
        &env,
        weaver_dir,
        mode,
        spec.claude_args,
    );
    tracing::debug!(
        workspace = spec.workspace_id,
        agent_kind = spec.agent_kind,
        session = spec.tmux_session,
        ?mode,
        "launching agent session"
    );
    tmux::new_session(spec.tmux_session, spec.work_dir, &script)
        .await
        .with_context(|| format!("tmux: launching session {}", spec.tmux_session))?;
    tracing::info!(
        workspace = spec.workspace_id,
        agent_kind = spec.agent_kind,
        session = spec.tmux_session,
        ?mode,
        "agent launched"
    );
    Ok(())
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
            "SessionStart": hook("session-start"),
            "UserPromptSubmit": hook("working"),
            "Notification": hook("waiting"),
            "Stop": hook("idle"),
        }
    })
}

/// The workspace primer, kept as a standalone markdown doc and catted in at
/// build time so `weaver hook` stays self-contained wherever it runs.
const PRIMER: &str = include_str!("../primer.md");

/// Context injected at SessionStart (via the `session-start` weaver hook): tells
/// the agent it is in a weaver workspace and how it is expected to behave.
///
/// Claude Code adds a SessionStart hook's JSON `additionalContext` to the
/// session, so this lands once per session — including after `/clear`.
pub fn session_primer() -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": PRIMER,
        }
    })
    .to_string()
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
    tracing::debug!(workspace = workspace_id, path = %path.display(), "claude hooks installed");
    Ok(())
}

const SUMMARY_PROMPT: &str = "You are summarizing the current state of a coding workspace for a \
status dashboard. The git diff of all work-in-progress is provided on stdin. Reply with 2-4 \
plain sentences describing what has changed so far and the apparent state of the work. No \
preamble, no markdown, no bullet points.";

const MAX_DIFF_CHARS: usize = 80_000;

/// Run a headless summary pass over a diff and return its summary text.
///
/// `command` is the configured `agent.summary_command` (default `claude`): its
/// first whitespace-separated token is the program and the rest are leading
/// arguments, after which `-p <prompt>` is appended. The diff is piped to
/// stdin. Extra arguments let the operator pick a model class, e.g.
/// `claude --model claude-haiku-4-5`.
pub async fn summarize(work_dir: &Path, command: &str, diff: &str) -> Result<String> {
    let mut diff = diff.to_string();
    if diff.len() > MAX_DIFF_CHARS {
        diff.truncate(MAX_DIFF_CHARS);
        diff.push_str("\n...[diff truncated]");
    }
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or("claude");
    let leading: Vec<&str> = parts.collect();
    tracing::debug!(
        dir = %work_dir.display(),
        diff_chars = diff.len(),
        %program,
        "running summary agent"
    );
    let mut child = Command::new(program)
        .args(&leading)
        .args(["-p", SUMMARY_PROMPT])
        .current_dir(work_dir)
        // Drop ANTHROPIC_API_KEY so the headless pass authenticates with the
        // user's Claude subscription (the OAuth login in ~/.claude) rather than
        // a pay-as-you-go API key — the server inherits whatever env it was
        // started with, and an API key with no credit balance fails the pass.
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn '{program}' (is it installed and on PATH?)"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes()).await;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait_with_output())
        .await
        .context("claude summary timed out")??;
    if !out.status.success() {
        // `claude -p` reports user-facing errors (e.g. "Credit balance is too
        // low") on stdout, not stderr — fall back to stdout so the failure is
        // never silent.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = match (stderr.trim(), stdout.trim()) {
            ("", "") => "no output".to_string(),
            ("", out) => out.to_string(),
            (err, "") => err.to_string(),
            (err, out) => format!("{err} | {out}"),
        };
        let code = out
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        let snippet: String = detail.chars().take(500).collect();
        tracing::warn!(
            dir = %work_dir.display(),
            code = %code,
            detail = %snippet,
            "claude summary failed"
        );
        anyhow::bail!("claude summary failed (exit {code}): {detail}");
    }
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    tracing::info!(
        dir = %work_dir.display(),
        summary_chars = summary.len(),
        "claude summary complete"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shell_script_just_execs_a_shell() {
        let script = launch_script("shell", None, &[], None, LaunchMode::Fresh, "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn claude_script_exports_env_and_runs_claude() {
        let script = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            &[("WEAVER_API", "http://h:1")],
            None,
            LaunchMode::Fresh,
            "",
        );
        assert!(script.contains("export WEAVER_API='http://h:1'; "));
        assert!(script.contains("claude \"$(cat '/x/goal.txt')\"; "));
        assert!(script.ends_with("exec \"${SHELL:-/bin/sh}\""));
    }

    #[test]
    fn claude_script_without_a_goal_runs_claude_bare() {
        let script = launch_script("claude", None, &[], None, LaunchMode::Fresh, "");
        assert_eq!(script, "claude; exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn adopt_mode_resumes_claude_with_continue() {
        // Adopt resumes the prior conversation and ignores the goal file —
        // re-seeding the goal would restart the agent from scratch.
        let script = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            &[],
            None,
            LaunchMode::Adopt,
            "",
        );
        assert_eq!(script, "claude --continue; exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn adopt_mode_for_shell_still_just_execs_a_shell() {
        let script = launch_script("shell", None, &[], None, LaunchMode::Adopt, "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn adopt_mode_relaunches_a_custom_agent() {
        let script = launch_script(
            "my-agent",
            Some(Path::new("/x/goal.txt")),
            &[],
            None,
            LaunchMode::Adopt,
            "",
        );
        assert_eq!(
            script,
            "my-agent '/x/goal.txt'; exec \"${SHELL:-/bin/sh}\""
        );
    }

    #[test]
    fn weaver_dir_is_prepended_to_path() {
        let script = launch_script(
            "shell",
            None,
            &[],
            Some(Path::new("/opt/bin")),
            LaunchMode::Fresh,
            "",
        );
        assert!(script.starts_with("export PATH=\"/opt/bin:$PATH\"; "));
    }

    #[test]
    fn claude_args_are_spliced_into_fresh_and_adopt() {
        let fresh = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            &[],
            None,
            LaunchMode::Fresh,
            "--model claude-opus-4-7",
        );
        assert_eq!(
            fresh,
            "claude --model claude-opus-4-7 \"$(cat '/x/goal.txt')\"; exec \"${SHELL:-/bin/sh}\""
        );
        let adopt = launch_script("claude", None, &[], None, LaunchMode::Adopt, "  --model x ");
        assert_eq!(adopt, "claude --continue --model x; exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn claude_args_are_ignored_by_non_claude_agents() {
        let script = launch_script(
            "my-agent",
            Some(Path::new("/x/goal.txt")),
            &[],
            None,
            LaunchMode::Fresh,
            "--model x",
        );
        assert_eq!(script, "my-agent '/x/goal.txt'; exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn hooks_point_at_the_weaver_binary() {
        let hooks = hooks_json("/usr/bin/weaver", "abc12345");
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(stop, "/usr/bin/weaver hook --workspace abc12345 --event idle");
    }

    #[test]
    fn session_start_hook_uses_a_distinct_event() {
        let hooks = hooks_json("/usr/bin/weaver", "abc12345");
        let cmd = hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(
            cmd,
            "/usr/bin/weaver hook --workspace abc12345 --event session-start"
        );
    }

    #[test]
    fn session_primer_is_session_start_additional_context() {
        let v: Value = serde_json::from_str(&session_primer()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert!(v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("weaver note"));
    }
}
