//! The `claude` builtin: Claude Code over its terminal CLI or the
//! `claude-agent-acp` adapter.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::{
    binary_on_path, effort_flag, join_args, model_flag, npm_adapter_cmd, spaced, AcpContract,
    BuiltinHarness,
};
use crate::agent::{
    install_hooks, loom_bin_path, sh_single_quote_path, start_terminal, AgentChoice,
    AgentLaunchContext, AgentMetadata, AgentType, LaunchMode,
};
use crate::db::Db;
use loom_ctx::agent_kind::{BuiltinAgentKind, DEFAULT_ACP_MODE};
use weaver_core::agent::HookMode;
use weaver_core::BoxFut;

pub static HARNESS: ClaudeAgentType = ClaudeAgentType;

const MODEL_CHOICES: &[(&str, &str)] = &[
    ("haiku", "Haiku"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("fable", "Fable"),
];

const EFFORT_CHOICES: &[(&str, &str)] = &[
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "X-High"),
    ("max", "Max"),
];

pub struct ClaudeAgentType;

impl AgentType for ClaudeAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "claude".to_string(),
            label: "Claude".to_string(),
            models: AgentChoice::list(MODEL_CHOICES),
            efforts: AgentChoice::list(EFFORT_CHOICES),
            accepts_raw_model: true,
            supports_hooks: true,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
            modes: self.acp_modes().into_iter().map(str::to_string).collect(),
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            prepare_claude(&ctx).await;
            start_terminal(&ctx, "claude", &claude_command(&ctx, LaunchMode::Fresh)).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            prepare_claude(&ctx).await;
            start_terminal(&ctx, "claude", &claude_command(&ctx, LaunchMode::Adopt)).await
        })
    }
}

impl BuiltinHarness for ClaudeAgentType {
    fn kind(&self) -> BuiltinAgentKind {
        BuiltinAgentKind::Claude
    }

    fn credential_names(&self) -> &'static [&'static str] {
        &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
    }

    fn available(&self) -> BoxFut<'static, bool> {
        Box::pin(binary_on_path("claude"))
    }

    fn acp_command(&self, db: Db) -> BoxFut<'static, String> {
        Box::pin(async move {
            // `WEAVER_CLAUDE_ACP_CMD` (env) wins, then the `acp.claude_cmd`
            // setting, else the npm default.
            if let Some(cmd) =
                super::acp_cmd_override(&db, "WEAVER_CLAUDE_ACP_CMD", "acp.claude_cmd").await
            {
                return cmd;
            }
            npm_adapter_cmd("claude-agent-acp", "@agentclientprotocol/claude-agent-acp")
        })
    }

    fn acp_mode_for(&self, mode: &str) -> Option<&'static str> {
        let mode = mode.trim();
        Some(match mode {
            "" => DEFAULT_ACP_MODE,
            "auto" => "auto",
            "default" => "default",
            "acceptEdits" => "acceptEdits",
            "plan" => "plan",
            "bypassPermissions" => "bypassPermissions",
            _ => return None,
        })
    }

    fn seeds_primer_as_goal(&self) -> bool {
        false
    }

    fn installs_prelude_hooks(&self) -> bool {
        true
    }

    fn acp_meta(&self, contract: &AcpContract<'_>) -> Option<Value> {
        acp_meta(
            contract.model,
            contract.primer_text,
            contract.mode,
            contract.restricted,
            contract.allowed_tools_json,
        )
    }
}

/// The `_meta.claudeCode.options` object for the claude adapter — only the
/// fields that are configured (model, the primer as `appendSystemPrompt`, the
/// permission mode). `None` when nothing is set.
fn acp_meta(
    model: &str,
    primer: Option<&str>,
    mode: &str,
    restricted: bool,
    allowed_tools_json: &str,
) -> Option<Value> {
    let mut options = Map::new();
    let model = model.trim();
    if !model.is_empty() {
        options.insert("model".to_string(), json!(model));
    }
    if let Some(p) = primer.map(str::trim).filter(|s| !s.is_empty()) {
        options.insert("appendSystemPrompt".to_string(), json!(p));
    }
    let mode = mode.trim();
    if !mode.is_empty() {
        options.insert("permissionMode".to_string(), json!(mode));
    }
    let allowed_tools: Vec<String> = serde_json::from_str(allowed_tools_json).unwrap_or_default();
    if restricted {
        let mut tools = Vec::<String>::new();
        for rule in &allowed_tools {
            let Some(name) = crate::profile::allowed_tool_name(rule) else {
                continue;
            };
            // MCP tools are contributed by the server below. `Read` rules also
            // govern Claude's built-in Glob/Grep paths, so expose that complete
            // read-only trio without adding unscoped allow rules.
            let visible = if name == "Read" {
                &["Read", "Glob", "Grep"][..]
            } else if name.starts_with("mcp__") {
                &[]
            } else {
                if !tools.iter().any(|existing| existing == name) {
                    tools.push(name.to_string());
                }
                continue;
            };
            for visible_name in visible {
                if !tools.iter().any(|existing| existing == visible_name) {
                    tools.push((*visible_name).to_string());
                }
            }
        }
        options.insert("allowedTools".to_string(), json!(allowed_tools));
        options.insert("tools".to_string(), json!(tools));
        options.insert("settingSources".to_string(), json!([]));
        options.insert("strictMcpConfig".to_string(), json!(true));
    }
    if options.is_empty() {
        return None;
    }
    Some(json!({ "claudeCode": { "options": options } }))
}

// -- terminal launch --------------------------------------------------------

/// The hook seed and the launch gates claude's first run would otherwise
/// stall on.
async fn prepare_claude(ctx: &AgentLaunchContext<'_>) {
    let loom_bin = loom_bin_path();

    if ctx.prelude == "weaver" {
        if let Err(e) = install_hooks(ctx.work_dir, &loom_bin, HookMode::Terminal).await {
            tracing::warn!(work_dir = %ctx.work_dir.display(), error = %e,
                "agent hook setup failed; launching without lifecycle hooks");
        }
    }
    if let Err(e) = seed_claude_launch_gates(ctx.work_dir, ctx.model, ctx.effort).await {
        tracing::warn!(work_dir = %ctx.work_dir.display(), error = %e,
            "agent launch-gate setup failed; agent may stall on first-run prompts");
    }
}

pub(crate) fn claude_command(ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = join_args([claude_model_arg(ctx.model), claude_effort_arg(ctx.effort)]);
    let args = spaced(args);
    match (mode, ctx.primer_file) {
        (LaunchMode::Adopt, Some(p)) => {
            format!(
                "claude --continue{args} --append-system-prompt-file {}",
                sh_single_quote_path(p)
            )
        }
        (LaunchMode::Fresh, Some(p)) => {
            format!(
                "claude{args} --append-system-prompt-file {}",
                sh_single_quote_path(p)
            )
        }
        (LaunchMode::Adopt, None) => format!("claude --continue{args}"),
        (LaunchMode::Fresh, None) => match ctx.goal_file {
            Some(f) => format!("claude{args} \"$(cat {})\"", sh_single_quote_path(f)),
            None => format!("claude{args}"),
        },
    }
}

fn claude_model_arg(model: &str) -> Option<String> {
    model_flag(model).map(|m| format!("--model {m}"))
}

fn claude_effort_arg(effort: &str) -> Option<String> {
    effort_flag(effort).map(|e| format!("--effort {e}"))
}

/// `--effort <level>` for a known level, else empty.
pub fn effort_args(effort: &str) -> String {
    claude_effort_arg(effort).unwrap_or_default()
}

/// `--model <tier>` for a chosen model, else empty.
pub fn model_args(model: &str) -> String {
    claude_model_arg(model).unwrap_or_default()
}

/// Combine per-session model and effort selections for the Claude protocol.
pub fn combine_args(model: &str, effort: &str) -> String {
    join_args([claude_model_arg(model), claude_effort_arg(effort)])
}

/// Where the claude CLI records its conversations (`~/.claude/projects`).
pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

/// The newest claude conversation recorded for `work_dir` under `projects_dir`:
/// claude munges the cwd into a directory name (every non-alphanumeric byte
/// becomes `-`) holding one `<session-id>.jsonl` per conversation. These are
/// the sessions `claude --continue` resumes, and the ACP adapter loads the same
/// ids, so an orphaned terminal session can adopt into ACP over its own
/// history. `None` when nothing is recorded for that directory.
pub fn latest_claude_session_id(projects_dir: &Path, work_dir: &Path) -> Option<String> {
    let munged: String = work_dir
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(projects_dir.join(munged)).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, stem.to_string()));
        }
    }
    newest.map(|(_, id)| id)
}

/// Pre-clear Claude Code's first-run interactive gates in the agent user's
/// global `~/.claude.json` so a detached, unattended `claude` runs its task
/// instead of stalling — or *quitting* — at a prompt no human can answer.
///
/// On a fresh, persisted container HOME these gates fire in sequence and each
/// wedges the session (it sits at "launching" with no `loom status`, worktree
/// idle). Each gate is state Claude records after a human answers once; we
/// write the same state ahead of time. Everything here is additive and
/// idempotent — only missing/false gates are set, existing config is preserved —
/// so it is safe to re-run before every launch. Gates handled:
///
/// * `hasCompletedOnboarding` + `theme` — the first-run theme picker.
/// * `projects.<repo-root>.hasTrustDialogAccepted` — the workspace-trust dialog.
///   Claude resolves a git worktree back to its **main repo root** and records
///   trust there, so trusting the root once covers every worktree under it.
/// * `customApiKeyResponses.approved` — the "use this `ANTHROPIC_API_KEY`?"
///   prompt, keyed by the key's last 20 chars; seeded only when that env var is
///   set (i.e. the agent authenticates by API key).
/// * `bypassPermissionsModeAccepted` — the one-time "you're in Bypass
///   Permissions mode" confirmation, **which defaults to *exit***. Seeded only
///   when this launch runs with a bypass flag, since the dialog only appears
///   then.
pub async fn seed_claude_launch_gates(work_dir: &Path, model: &str, effort: &str) -> Result<()> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        tracing::debug!("HOME unset; skipping claude launch-gate seed");
        return Ok(());
    };
    let path = home.join(".claude.json");
    // Read any existing config, distinguishing "absent" (fine — start fresh) from
    // "present but unparseable" (bail). On a parse error we must NOT fall back to
    // `{}` and write: that would clobber a real config that's momentarily
    // truncated or mid-write (e.g. a concurrent `claude` writing the file).
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(s) if s.trim().is_empty() => json!({}),
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "~/.claude.json present but unparseable; skipping launch-gate \
                     seed rather than overwriting it");
                return Ok(());
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let launch_args = combine_args(model, effort);
    let bypass = launch_args.contains("--dangerously-skip-permissions")
        || launch_args.contains("bypassPermissions");
    // Last 20 chars, long enough to slice (see doc comment above).
    let api_key_tail = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| k.len() >= 20)
        .map(|k| k[k.len() - 20..].to_string());
    let repo_root = match weaver_core::git::repo_root(work_dir).await {
        Ok(r) => Some(r.to_string_lossy().into_owned()),
        Err(e) => {
            tracing::debug!(work_dir = %work_dir.display(), error = %e,
                "could not resolve repo root for trust seed");
            None
        }
    };

    let seed = GateSeed {
        bypass,
        api_key_tail: api_key_tail.as_deref(),
        repo_root: repo_root.as_deref(),
    };
    if !apply_launch_gates(&mut root, &seed) {
        return Ok(());
    }

    tokio::fs::write(&path, serde_json::to_string_pretty(&root)?)
        .await
        .with_context(|| format!("seeding {}", path.display()))?;
    // claude writes this file 0600; preserve that posture.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
    }
    tracing::info!(path = %path.display(), bypass, "seeded claude launch gates");
    Ok(())
}

/// The environment-derived inputs for [`apply_launch_gates`], gathered by
/// [`seed_claude_launch_gates`] so the merge itself is a pure function.
struct GateSeed<'a> {
    bypass: bool,
    /// `ANTHROPIC_API_KEY`'s last 20 chars, if set.
    api_key_tail: Option<&'a str>,
    repo_root: Option<&'a str>,
}

/// Merge the first-run gate state into a parsed `.claude.json` value. Pure,
/// additive, and idempotent: only missing or `false` keys are written and every
/// existing value is preserved, so re-running is a no-op and a user's real config
/// is never clobbered. Returns whether anything changed. Split out from
/// [`seed_claude_launch_gates`] (which does the env/fs/git I/O) so these merge
/// paths are unit-testable without touching HOME or a git repo.
fn apply_launch_gates(root: &mut Value, seed: &GateSeed) -> bool {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("root is an object");
    let mut changed = false;

    // 1. Onboarding / theme picker.
    if obj.get("hasCompletedOnboarding").and_then(Value::as_bool) != Some(true) {
        obj.insert("hasCompletedOnboarding".into(), json!(true));
        changed = true;
    }
    if !obj.contains_key("theme") {
        obj.insert("theme".into(), json!("dark"));
        changed = true;
    }

    // 2. Bypass-permissions acceptance — only when we launch in that mode.
    if seed.bypass
        && obj
            .get("bypassPermissionsModeAccepted")
            .and_then(Value::as_bool)
            != Some(true)
    {
        obj.insert("bypassPermissionsModeAccepted".into(), json!(true));
        changed = true;
    }

    // 3. Ambient ANTHROPIC_API_KEY approval (keyed by the key's last 20 chars).
    if let Some(tail) = seed.api_key_tail {
        let entry = obj
            .entry("customApiKeyResponses")
            .or_insert_with(|| json!({"approved": [], "rejected": []}));
        if !entry.is_object() {
            *entry = json!({"approved": [], "rejected": []});
        }
        let entry = entry.as_object_mut().unwrap();
        if !entry.get("approved").map(Value::is_array).unwrap_or(false) {
            entry.insert("approved".into(), json!([]));
        }
        let approved = entry.get_mut("approved").unwrap().as_array_mut().unwrap();
        if !approved.iter().any(|v| v.as_str() == Some(tail)) {
            approved.push(json!(tail));
            changed = true;
        }
        if !entry.contains_key("rejected") {
            entry.insert("rejected".into(), json!([]));
        }
    }

    // 4. Workspace trust, recorded at the worktree's main repo root.
    if let Some(repo_root) = seed.repo_root {
        let projects = obj.entry("projects").or_insert_with(|| json!({}));
        if !projects.is_object() {
            *projects = json!({});
        }
        let proj = projects
            .as_object_mut()
            .unwrap()
            .entry(repo_root.to_string())
            .or_insert_with(|| json!({}));
        if !proj.is_object() {
            *proj = json!({});
        }
        let proj = proj.as_object_mut().unwrap();
        if proj.get("hasTrustDialogAccepted").and_then(Value::as_bool) != Some(true) {
            proj.insert("hasTrustDialogAccepted".into(), json!(true));
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed<'a>(
        bypass: bool,
        api_key_tail: Option<&'a str>,
        repo_root: Option<&'a str>,
    ) -> GateSeed<'a> {
        GateSeed {
            bypass,
            api_key_tail,
            repo_root,
        }
    }

    #[test]
    fn seeds_all_gates_into_an_empty_config() {
        let mut root = json!({});
        assert!(apply_launch_gates(
            &mut root,
            &seed(true, Some("KEYTAIL0123456789abc"), Some("/repo"))
        ));
        assert_eq!(root["hasCompletedOnboarding"], json!(true));
        assert_eq!(root["theme"], json!("dark"));
        assert_eq!(root["bypassPermissionsModeAccepted"], json!(true));
        assert_eq!(
            root["customApiKeyResponses"]["approved"],
            json!(["KEYTAIL0123456789abc"])
        );
        assert_eq!(
            root["projects"]["/repo"]["hasTrustDialogAccepted"],
            json!(true)
        );
    }

    #[test]
    fn is_idempotent_and_returns_false_on_a_second_pass() {
        let mut root = json!({});
        let s = seed(true, Some("KEYTAIL0123456789abc"), Some("/repo"));
        assert!(apply_launch_gates(&mut root, &s));
        let after_first = root.clone();
        // A second pass changes nothing and reports no change.
        assert!(!apply_launch_gates(&mut root, &s));
        assert_eq!(root, after_first);
        // The approved key is not duplicated.
        assert_eq!(
            root["customApiKeyResponses"]["approved"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn preserves_existing_user_config() {
        // A real config the user already has: a chosen theme, an unrelated
        // approved key, and an unrelated trusted project with extra fields.
        let mut root = json!({
            "theme": "light",
            "hasCompletedOnboarding": true,
            "customApiKeyResponses": { "approved": ["existing-key"], "rejected": ["nope"] },
            "projects": { "/other": { "hasTrustDialogAccepted": true, "keep": 1 } },
        });
        assert!(apply_launch_gates(
            &mut root,
            &seed(false, Some("KEYTAIL0123456789abc"), Some("/repo"))
        ));
        // Existing values untouched...
        assert_eq!(root["theme"], json!("light"));
        assert_eq!(root["projects"]["/other"]["keep"], json!(1));
        assert_eq!(root["customApiKeyResponses"]["rejected"], json!(["nope"]));
        // ...new approved key appended alongside the existing one...
        assert_eq!(
            root["customApiKeyResponses"]["approved"],
            json!(["existing-key", "KEYTAIL0123456789abc"])
        );
        // ...new project trusted, and (bypass=false) no bypass key written.
        assert_eq!(
            root["projects"]["/repo"]["hasTrustDialogAccepted"],
            json!(true)
        );
        assert!(root.get("bypassPermissionsModeAccepted").is_none());
    }

    #[test]
    fn omits_optional_gates_when_inputs_absent() {
        let mut root = json!({});
        assert!(apply_launch_gates(&mut root, &seed(false, None, None)));
        // Onboarding/theme always seed; the env-dependent gates do not.
        assert_eq!(root["hasCompletedOnboarding"], json!(true));
        assert!(root.get("bypassPermissionsModeAccepted").is_none());
        assert!(root.get("customApiKeyResponses").is_none());
        assert!(root.get("projects").is_none());
    }

    #[test]
    fn replaces_a_non_object_root() {
        // A non-object root is reset rather than panicking.
        let mut root = json!("not an object");
        assert!(apply_launch_gates(&mut root, &seed(false, None, None)));
        assert!(root.is_object());
        assert_eq!(root["hasCompletedOnboarding"], json!(true));
    }

    #[test]
    fn acp_meta_sets_only_configured_fields() {
        let none = acp_meta("", None, "", false, "[]");
        assert!(none.is_none());

        let model_only = acp_meta(" sonnet ", None, "", false, "[]").unwrap();
        assert_eq!(model_only["claudeCode"]["options"]["model"], "sonnet");

        let full = acp_meta(
            "sonnet",
            Some("  "),
            " acceptEdits ",
            true,
            r#"["Read","Bash(git status:*)","mcp__loom__status"]"#,
        )
        .unwrap();
        let options = &full["claudeCode"]["options"];
        assert_eq!(options["model"], "sonnet");
        assert_eq!(options["permissionMode"], "acceptEdits");
        assert!(options.get("appendSystemPrompt").is_none());
        // MCP tools come from the server; a Read rule exposes the read trio
        // and a Bash rule names its bare tool.
        assert_eq!(
            options["tools"],
            serde_json::json!(["Read", "Glob", "Grep", "Bash"])
        );
        assert_eq!(options["settingSources"], serde_json::json!([]));
        assert_eq!(options["strictMcpConfig"], serde_json::json!(true));
    }

    #[test]
    fn claude_offers_every_loom_posture() {
        assert_eq!(
            HARNESS.acp_modes(),
            vec![
                "auto",
                "default",
                "acceptEdits",
                "plan",
                "bypassPermissions"
            ]
        );
    }
}
