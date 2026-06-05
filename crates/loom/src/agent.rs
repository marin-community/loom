//! Launching coding agents into tmux panes, installing status hooks, and
//! running headless summary passes.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::tmux;
use weaver_core::agent::hooks_json;

/// Accepted per-session reasoning effort levels, increasing. Orthogonal to the
/// model: any model can run at any effort.
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Accepted per-session model tiers.
pub const MODELS: &[&str] = &["haiku", "sonnet", "opus"];

/// `--effort <level>` for a known level, else empty (inherit the configured
/// `agent.claude_args`).
pub fn effort_args(effort: &str) -> String {
    match effort.trim() {
        "" => String::new(),
        e => format!("--effort {e}"),
    }
}

/// `--model <tier>` for a chosen model, else empty.
pub fn model_args(model: &str) -> String {
    match model.trim() {
        "" => String::new(),
        m => format!("--model {m}"),
    }
}

/// Combine the configured base `agent.claude_args` with the per-session model
/// and effort selections. Each non-empty part is appended in turn, so a session
/// layers its model/effort on top of the global flags.
pub fn combine_args(base: &str, model: &str, effort: &str) -> String {
    [base.trim(), &model_args(model), &effort_args(effort)]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a session's tmux session is being created for the first time or
/// recreated to recover ("adopt") an existing worktree whose session died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// First launch: seed the agent with the branch's goal.
    Fresh,
    /// Re-launch into an existing worktree: resume rather than restart.
    Adopt,
}

fn inner_command(
    agent_kind: &str,
    goal_file: Option<&Path>,
    mode: LaunchMode,
    claude_args: &str,
) -> String {
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

pub fn launch_script(
    agent_kind: &str,
    goal_file: Option<&Path>,
    env: &[(&str, &str)],
    weaver_dir: Option<&Path>,
    mode: LaunchMode,
    claude_args: &str,
) -> String {
    let mut script = String::new();
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

/// Everything [`launch`] needs to bring up a session's tmux.
pub struct LaunchSpec<'a> {
    /// The branch id — the agent uses this to resolve "its" branch via
    /// `$WEAVER_BRANCH`.
    pub branch_id: &'a str,
    pub agent_kind: &'a str,
    pub work_dir: &'a Path,
    pub tmux_session: &'a str,
    pub goal_file: Option<&'a Path>,
    pub server_addr: &'a str,
    pub claude_args: &'a str,
}

/// Bring up the session's tmux running the agent.
pub async fn launch(spec: &LaunchSpec<'_>, mode: LaunchMode) -> Result<()> {
    let loom_exe = std::env::current_exe().ok();
    let weaver_dir = loom_exe.as_deref().and_then(Path::parent);
    let weaver_bin = weaver_dir
        .map(|d| d.join("weaver").display().to_string())
        .unwrap_or_else(|| "weaver".to_string());

    if spec.agent_kind == "claude" {
        install_hooks(spec.work_dir, &weaver_bin).await.ok();
    }

    let api_url = format!("http://{}", spec.server_addr);
    let env = [
        ("WEAVER_API", api_url.as_str()),
        ("WEAVER_BRANCH", spec.branch_id),
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
        branch = spec.branch_id,
        agent_kind = spec.agent_kind,
        session = spec.tmux_session,
        ?mode,
        "launching agent session"
    );
    tmux::new_session(spec.tmux_session, spec.work_dir, &script)
        .await
        .with_context(|| format!("tmux: launching session {}", spec.tmux_session))?;
    tracing::info!(
        branch = spec.branch_id,
        agent_kind = spec.agent_kind,
        session = spec.tmux_session,
        ?mode,
        "agent launched"
    );
    Ok(())
}

/// Write (merging into any existing file) `.claude/settings.local.json` so the
/// agent reports status to weaver via hooks.
pub async fn install_hooks(work_dir: &Path, weaver_bin: &str) -> Result<()> {
    let dir = work_dir.join(".claude");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("settings.local.json");
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    let hooks = hooks_json(weaver_bin);
    root["hooks"] = hooks["hooks"].clone();
    tokio::fs::write(&path, serde_json::to_string_pretty(&root)?).await?;
    tracing::debug!(path = %path.display(), "claude hooks installed");
    Ok(())
}

const SUMMARY_PROMPT: &str = "You are summarizing the current state of a coding session for a \
status dashboard. The git diff of all work-in-progress is provided on stdin. Reply with 2-4 \
plain sentences describing what has changed so far and the apparent state of the work. No \
preamble, no markdown, no bullet points.";

const MAX_DIFF_CHARS: usize = 80_000;

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
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn '{program}' (is it installed and on PATH?)"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes()).await;
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        child.wait_with_output(),
    )
    .await
    .context("claude summary timed out")??;
    if !out.status.success() {
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
    fn effort_and_model_args() {
        assert_eq!(effort_args("xhigh"), "--effort xhigh");
        assert_eq!(effort_args(""), "");
        assert_eq!(model_args("opus"), "--model opus");
        assert_eq!(model_args(""), "");
    }

    #[test]
    fn combine_args_layers_model_and_effort_onto_base() {
        assert_eq!(
            combine_args("", "opus", "high"),
            "--model opus --effort high"
        );
        assert_eq!(combine_args("--verbose", "", ""), "--verbose");
        assert_eq!(combine_args("", "", "max"), "--effort max");
        assert_eq!(combine_args("", "haiku", ""), "--model haiku");
        assert_eq!(
            combine_args("--verbose", "sonnet", "low"),
            "--verbose --model sonnet --effort low"
        );
        assert_eq!(combine_args("", "", ""), "");
    }

    #[test]
    fn adopt_mode_resumes_claude_with_continue() {
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
}
