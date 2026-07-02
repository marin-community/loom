//! Launching coding agents into per-session terminals, plus the **one-shot
//! headless agent** (`POST /api/agent/oneshot`) — a fresh, env-stripped agent
//! run for a judgement call.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::backend;
use crate::custom_agents::CustomAgent;
use crate::db::Db;
use weaver_core::agent::hooks_json;

#[derive(Debug, Clone, Serialize)]
pub struct AgentChoice {
    pub id: String,
    pub label: String,
}

impl AgentChoice {
    /// Materialize a builtin's static `(id, label)` table into the owned choices
    /// the metadata carries.
    fn list(pairs: &[(&str, &str)]) -> Vec<AgentChoice> {
        pairs
            .iter()
            .map(|(id, label)| AgentChoice {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect()
    }
}

/// What the agent picker and the settings validators need to know about one
/// agent: its id, label, the model/effort choices it offers, and a few capability
/// flags. Built fresh per call, so it can describe a DB-backed custom agent as
/// easily as a builtin.
#[derive(Debug, Clone, Serialize)]
pub struct AgentMetadata {
    pub kind: String,
    pub label: String,
    pub models: Vec<AgentChoice>,
    pub efforts: Vec<AgentChoice>,
    pub accepts_raw_model: bool,
    pub supports_hooks: bool,
    pub supports_concierge: bool,
    /// True for the code-shipped `claude`/`codex`; false for an operator-defined
    /// custom agent (which the UI may edit or delete).
    pub builtin: bool,
}

pub struct AgentInstance {
    pub term_session: String,
}

pub type AgentFuture<'a> = Pin<Box<dyn Future<Output = Result<AgentInstance>> + Send + 'a>>;

pub trait AgentType: Sync {
    fn metadata(&self) -> AgentMetadata;

    fn validate(&self, model: &str, effort: &str) -> Result<(), String> {
        let metadata = self.metadata();
        validate_model(&metadata, model)?;
        validate_effort(&metadata, effort)
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a>;
    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a>;
}

pub struct ClaudeAgentType;
pub struct CodexAgentType;

/// A [`CustomAgent`] row wrapped as an [`AgentType`]: its stages drive the launch
/// script, and its metadata is derived from the stored fields.
pub struct CustomAgentType {
    agent: CustomAgent,
}

impl CustomAgentType {
    pub fn new(agent: CustomAgent) -> Self {
        Self { agent }
    }
}

const MODEL_CHOICES: &[(&str, &str)] = &[
    ("haiku", "Haiku"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("fable", "Fable"),
];

const CODEX_MODEL_CHOICES: &[(&str, &str)] = &[
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4 Mini"),
    ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
];

const EFFORT_CHOICES: &[(&str, &str)] = &[
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "X-High"),
    ("max", "Max"),
];

const CODEX_EFFORT_CHOICES: &[(&str, &str)] = &[
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "X-High"),
];

static CLAUDE_AGENT_TYPE: ClaudeAgentType = ClaudeAgentType;
static CODEX_AGENT_TYPE: CodexAgentType = CodexAgentType;

/// The builtin agent for `kind`, if it names one (`claude`/`codex`). Custom
/// agents live in the database and are resolved by [`resolve`]; this covers only
/// the code-shipped runtimes, which need no DB lookup.
pub fn builtin_agent_type(kind: &str) -> Option<&'static dyn AgentType> {
    match kind {
        "claude" => Some(&CLAUDE_AGENT_TYPE),
        "codex" => Some(&CODEX_AGENT_TYPE),
        _ => None,
    }
}

/// The builtin agents' metadata, in picker order.
pub fn builtin_metadata() -> Vec<AgentMetadata> {
    [&CLAUDE_AGENT_TYPE as &dyn AgentType, &CODEX_AGENT_TYPE]
        .into_iter()
        .map(AgentType::metadata)
        .collect()
}

/// A launchable agent resolved from a kind: either a builtin static type or a
/// database-backed custom agent (owned, since it carries the row's commands).
pub enum ResolvedAgent {
    Builtin(&'static dyn AgentType),
    Custom(CustomAgentType),
}

impl ResolvedAgent {
    pub fn as_type(&self) -> &dyn AgentType {
        match self {
            ResolvedAgent::Builtin(t) => *t,
            ResolvedAgent::Custom(c) => c,
        }
    }
}

/// Resolve `kind` to a launchable agent: a builtin first, then a custom agent
/// from the `custom_agents` table. `Ok(None)` means no agent by that name.
pub async fn resolve(db: &Db, kind: &str) -> Result<Option<ResolvedAgent>> {
    if let Some(t) = builtin_agent_type(kind) {
        return Ok(Some(ResolvedAgent::Builtin(t)));
    }
    Ok(crate::custom_agents::get(db, kind)
        .await?
        .map(|a| ResolvedAgent::Custom(CustomAgentType::new(a))))
}

/// Every agent's metadata — the builtins followed by the operator's custom agents
/// (name order). What `GET /api/agents` lists and the picker renders.
pub async fn agent_metadata(db: &Db) -> Result<Vec<AgentMetadata>> {
    let mut out = builtin_metadata();
    for a in crate::custom_agents::list(db).await? {
        out.push(CustomAgentType::new(a).metadata());
    }
    Ok(out)
}

/// The metadata for one agent kind, or `None` when it names no agent.
pub async fn metadata_for(db: &Db, kind: &str) -> Result<Option<AgentMetadata>> {
    Ok(resolve(db, kind).await?.map(|r| r.as_type().metadata()))
}

/// Whether `kind` names a known agent (builtin or custom).
pub async fn exists(db: &Db, kind: &str) -> bool {
    matches!(metadata_for(db, kind).await, Ok(Some(_)))
}

/// The lifecycle status a freshly launched session of `runtime` starts in. An
/// agent that fires weaver's hooks starts `launching` (its SessionStart/work hook
/// promotes it to `running`); a hookless agent (codex, most custom agents) never
/// gets that hook, so it is `running` from the start rather than stuck
/// `launching`. An unknown runtime is treated as hookless.
pub async fn initial_status(db: &Db, runtime: &str) -> &'static str {
    let hooked = matches!(metadata_for(db, runtime).await, Ok(Some(m)) if m.supports_hooks);
    if hooked {
        "launching"
    } else {
        "running"
    }
}

/// Check that `model` is one of `metadata`'s offered choices (blank is always
/// allowed — it means the agent's own default). A key-free reason on mismatch.
pub fn validate_model(metadata: &AgentMetadata, model: &str) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(());
    }
    if metadata.models.iter().any(|choice| choice.id == model) {
        Ok(())
    } else {
        Err(format!("unknown model '{model}' for {}", metadata.kind))
    }
}

/// Check that `effort` is one of `metadata`'s offered choices (blank is always
/// allowed — the agent's own default). A key-free reason on mismatch.
pub fn validate_effort(metadata: &AgentMetadata, effort: &str) -> Result<(), String> {
    let effort = effort.trim();
    if effort.is_empty() {
        return Ok(());
    }
    if metadata.efforts.iter().any(|choice| choice.id == effort) {
        Ok(())
    } else {
        Err(format!("unknown effort '{effort}' for {}", metadata.kind))
    }
}

fn model_flag(model: &str) -> Option<&str> {
    let model = model.trim();
    (!model.is_empty()).then_some(model)
}

fn effort_flag(effort: &str) -> Option<&str> {
    let effort = effort.trim();
    (!effort.is_empty()).then_some(effort)
}

fn claude_model_arg(model: &str) -> Option<String> {
    model_flag(model).map(|m| format!("--model {m}"))
}

fn claude_effort_arg(effort: &str) -> Option<String> {
    effort_flag(effort).map(|e| format!("--effort {e}"))
}

fn codex_model_arg(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some(format!("--model {model}"))
}

fn codex_effort_arg(effort: &str) -> Option<String> {
    effort_flag(effort).map(|e| format!("-c model_reasoning_effort=\\\"{e}\\\""))
}

fn join_args(args: impl IntoIterator<Item = Option<String>>) -> String {
    args.into_iter().flatten().collect::<Vec<_>>().join(" ")
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

fn claude_command(ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = join_args([claude_model_arg(ctx.model), claude_effort_arg(ctx.effort)]);
    let args = if args.is_empty() {
        String::new()
    } else {
        format!(" {args}")
    };
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

fn codex_command(ctx: &AgentLaunchContext<'_>) -> String {
    let args = join_args([codex_model_arg(ctx.model), codex_effort_arg(ctx.effort)]);
    let args = if args.is_empty() {
        String::new()
    } else {
        format!(" {args}")
    };
    match ctx.goal_file.or(ctx.primer_file) {
        Some(f) => format!("codex{args} \"$(cat {})\"", sh_single_quote_path(f)),
        None => format!("codex{args}"),
    }
}

/// The inner launch command for a custom agent: its `setup` stage (if any), then
/// the stage command for this `mode`. Fresh runs `launch` with the goal file
/// appended as a positional argument (mirroring the builtin runtimes); adopt runs
/// `resume` with no goal, falling back to `launch`-with-goal when `resume` is
/// blank. An empty result execs a bare shell — a setup-only or command-less agent.
fn custom_command(agent: &CustomAgent, ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let launch_with_goal = |cmd: &str| match ctx.goal_file.or(ctx.primer_file) {
        // The goal *content* is passed as a positional argument, mirroring the
        // builtin runtimes (`claude "$(cat …)"`), not the file path.
        Some(f) if !cmd.is_empty() => format!("{cmd} \"$(cat {})\"", sh_single_quote_path(f)),
        _ => cmd.to_string(),
    };
    let command = match mode {
        LaunchMode::Fresh => launch_with_goal(agent.launch.trim()),
        LaunchMode::Adopt => {
            let resume = agent.resume.trim();
            if resume.is_empty() {
                launch_with_goal(agent.launch.trim())
            } else {
                resume.to_string()
            }
        }
    };
    join_shell(&[agent.setup.trim(), command.as_str()])
}

/// Join non-empty shell fragments with `; ` so they run in sequence.
fn join_shell(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Whether a session's terminal is being created for the first time or
/// recreated to recover ("adopt") an existing worktree whose session died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// First launch: seed the agent with the branch's goal.
    Fresh,
    /// Re-launch into an existing worktree: resume rather than restart.
    Adopt,
}

pub struct AgentLaunchContext<'a> {
    /// The branch id — the agent uses this to resolve "its" branch via
    /// `$WEAVER_BRANCH`.
    pub branch_id: &'a str,
    pub work_dir: &'a Path,
    pub term_session: &'a str,
    /// The positional opening prompt catted in as the operator's first message.
    pub goal_file: Option<&'a Path>,
    /// Optional system context file for runtimes that support it.
    pub primer_file: Option<&'a Path>,
    pub server_addr: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// Operator-managed environment variables exported into the session.
    pub extra_env: &'a [(String, String)],
}

impl AgentType for ClaudeAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "claude".to_string(),
            label: "Claude".to_string(),
            models: AgentChoice::list(MODEL_CHOICES),
            efforts: AgentChoice::list(EFFORT_CHOICES),
            accepts_raw_model: false,
            supports_hooks: true,
            supports_concierge: true,
            builtin: true,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            prepare_claude(&ctx).await;
            start_terminal(&ctx, "claude", &claude_command(&ctx, LaunchMode::Fresh)).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            prepare_claude(&ctx).await;
            start_terminal(&ctx, "claude", &claude_command(&ctx, LaunchMode::Adopt)).await
        })
    }
}

impl AgentType for CodexAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "codex".to_string(),
            label: "Codex".to_string(),
            models: AgentChoice::list(CODEX_MODEL_CHOICES),
            efforts: AgentChoice::list(CODEX_EFFORT_CHOICES),
            accepts_raw_model: false,
            supports_hooks: false,
            supports_concierge: true,
            builtin: true,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }
}

impl AgentType for CustomAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: self.agent.name.clone(),
            label: self.agent.label.clone(),
            // Custom agents don't expose model/effort pickers — the operator bakes
            // any such flags into the stage commands themselves.
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: false,
            supports_hooks: self.agent.reports_status,
            supports_concierge: false,
            builtin: false,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            let inner = custom_command(&self.agent, &ctx, LaunchMode::Fresh);
            start_terminal(&ctx, &self.agent.name, &inner).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            let inner = custom_command(&self.agent, &ctx, LaunchMode::Adopt);
            start_terminal(&ctx, &self.agent.name, &inner).await
        })
    }
}

/// The agent kind that marks a session as the fleet **concierge** — seeded with
/// the [`concierge_primer`] instead of a workstream goal, hidden from the fleet
/// list, and resolved as a singleton by the Chat surface. This is the session's
/// *role*; the runtime it actually launches (claude|codex) is the separate
/// `concierge.runtime` setting, resolved before launch.
pub const CONCIERGE_KIND: &str = "concierge";

/// The builtin concierge primer — how the fleet concierge explores and acts on
/// the fleet. Catted in at build time like [`weaver_core::agent::builtin_weaver_md`],
/// and used as a concierge session's opening prompt.
const BUILTIN_CONCIERGE_MD: &str = include_str!("../CONCIERGE.md");

/// The concierge primer text, seeded as a concierge session's opening prompt.
pub fn concierge_primer() -> &'static str {
    BUILTIN_CONCIERGE_MD
}

/// Wrap a value in single quotes for safe `export NAME=…` in the launch script,
/// escaping any embedded single quote the POSIX way (`'\''` — close the quote,
/// an escaped literal quote, reopen). loom's own values never contain quotes,
/// but operator-supplied env vars ([`crate::agent_env`]) are arbitrary, so a
/// stray `'` must not break out of the assignment.
fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sh_single_quote_path(path: &Path) -> String {
    sh_single_quote(&path.display().to_string())
}

fn wrap_launch_script(inner: &str, env: &[(&str, &str)], weaver_dir: Option<&Path>) -> String {
    let mut script = String::new();
    if let Some(dir) = weaver_dir {
        script.push_str(&format!("export PATH=\"{}:$PATH\"; ", dir.display()));
    }
    for (k, v) in env {
        script.push_str(&format!("export {k}={}; ", sh_single_quote(v)));
    }
    if !inner.is_empty() {
        script.push_str(inner);
        script.push_str("; ");
    }
    script.push_str("exec \"${SHELL:-/bin/sh}\"");
    script
}

/// The launch script for a **bare login shell** — loom's `env` exported, then
/// `exec` the shell, with no inner agent command. Used by the operator scratch
/// shell and per-session debug shells ([`crate::shell`]); those are plain shells,
/// not agents, so they don't go through [`launch`] or an [`AgentType`].
pub fn bare_shell_script(env: &[(&str, &str)], weaver_dir: Option<&Path>) -> String {
    wrap_launch_script("", env, weaver_dir)
}

/// Everything [`launch`] needs to bring up a session's terminal.
pub struct LaunchSpec<'a> {
    /// The branch id — the agent uses this to resolve "its" branch via
    /// `$WEAVER_BRANCH`.
    pub branch_id: &'a str,
    /// The resolved **runtime** to launch — a builtin (`claude`/`codex`) or a
    /// custom agent's name, already resolved from the stored kind (so a concierge
    /// carries its `concierge.runtime`, not the literal `concierge`).
    pub runtime: &'a str,
    pub work_dir: &'a Path,
    pub term_session: &'a str,
    /// The **positional** opening prompt catted in as the operator's first
    /// message. A normal session carries this; the concierge does not (it uses
    /// [`Self::primer_file`] instead, so it boots idle).
    pub goal_file: Option<&'a Path>,
    /// System *context* appended via `--append-system-prompt-file` (claude
    /// runtime). The concierge's fleet-ops primer rides in here so it boots primed
    /// but takes no turn until the operator speaks. Distinct from `goal_file`;
    /// at most one is set.
    pub primer_file: Option<&'a Path>,
    pub server_addr: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// Operator-managed environment variables ([`crate::agent_env`]) exported
    /// into the session on top of loom's own `WEAVER_*` / `LOOM_TOKEN`. The
    /// caller reads these from the database; an empty slice adds nothing.
    pub extra_env: &'a [(String, String)],
}

/// Bring up the session's terminal running the agent. `spec.runtime` is resolved
/// through [`resolve`] — a builtin (`claude`/`codex`) or a custom agent from the
/// `custom_agents` table — so an unknown runtime is a hard error rather than a
/// silently-mistyped bare command.
pub async fn launch(db: &Db, spec: &LaunchSpec<'_>, mode: LaunchMode) -> Result<()> {
    let ctx = AgentLaunchContext {
        branch_id: spec.branch_id,
        work_dir: spec.work_dir,
        term_session: spec.term_session,
        goal_file: spec.goal_file,
        primer_file: spec.primer_file,
        server_addr: spec.server_addr,
        model: spec.model,
        effort: spec.effort,
        extra_env: spec.extra_env,
    };
    let resolved = resolve(db, spec.runtime)
        .await?
        .ok_or_else(|| anyhow!("unknown agent '{}'", spec.runtime))?;
    let agent_type = resolved.as_type();
    let _instance = match mode {
        LaunchMode::Fresh => agent_type.create(ctx).await,
        LaunchMode::Adopt => agent_type.adopt(ctx).await,
    }?;
    Ok(())
}

async fn prepare_claude(ctx: &AgentLaunchContext<'_>) {
    let loom_exe = std::env::current_exe().ok();
    let weaver_dir = loom_exe.as_deref().and_then(Path::parent);
    let weaver_bin = weaver_dir
        .map(|d| d.join("weaver").display().to_string())
        .unwrap_or_else(|| "weaver".to_string());

    if let Err(e) = install_hooks(ctx.work_dir, &weaver_bin).await {
        tracing::warn!(work_dir = %ctx.work_dir.display(), error = %e,
            "agent hook setup failed; launching without lifecycle hooks");
    }
    if let Err(e) = seed_claude_launch_gates(ctx.work_dir, ctx.model, ctx.effort).await {
        tracing::warn!(work_dir = %ctx.work_dir.display(), error = %e,
            "agent launch-gate setup failed; agent may stall on first-run prompts");
    }
}

async fn start_terminal(
    ctx: &AgentLaunchContext<'_>,
    runtime: &str,
    inner: &str,
) -> Result<AgentInstance> {
    let loom_exe = std::env::current_exe().ok();
    let weaver_dir = loom_exe.as_deref().and_then(Path::parent);
    let api_url = format!("http://{}", ctx.server_addr);
    // Hand the agent the machine-local token so its in-worktree `loom session …`
    // calls authenticate even when loopback trust is off. Absent file ⇒ omit it
    // (loopback trust then covers the local case).
    let local_token = read_local_token();
    let mut env = vec![
        ("WEAVER_API", api_url.as_str()),
        ("WEAVER_BRANCH", ctx.branch_id),
    ];
    if let Some(token) = local_token.as_deref() {
        env.push(("LOOM_TOKEN", token));
    }
    // Operator-managed vars are exported last, so for any shared name they'd win.
    // That's safe because `agent_env::validate_name` reserves loom's own
    // WEAVER_*/LOOM_ prefixes, so a stored var can never shadow the environment
    // loom needs — everything else the agent's tools read is theirs to set.
    for (k, v) in ctx.extra_env {
        env.push((k.as_str(), v.as_str()));
    }
    let script = wrap_launch_script(inner, &env, weaver_dir);
    tracing::debug!(
        branch = ctx.branch_id,
        runtime,
        session = ctx.term_session,
        "launching agent session"
    );
    backend::new_session(ctx.term_session, ctx.work_dir, &script)
        .await
        .with_context(|| format!("terminal: launching session {}", ctx.term_session))?;
    tracing::info!(
        branch = ctx.branch_id,
        runtime,
        session = ctx.term_session,
        "agent launched"
    );
    Ok(AgentInstance {
        term_session: ctx.term_session.to_string(),
    })
}

/// The machine-local bearer token (trimmed), if the daemon has minted it. Read
/// straight off disk so callers needn't thread it through; absent ⇒ `None`.
pub fn read_local_token() -> Option<String> {
    std::fs::read_to_string(crate::auth::local_token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

/// Pre-clear Claude Code's first-run interactive gates in the agent user's
/// global `~/.claude.json` so a detached, unattended `claude` runs its task
/// instead of stalling — or *quitting* — at a prompt no human can answer.
///
/// On a fresh, persisted container HOME these gates fire in sequence and each
/// wedges the session (it sits at "launching" with no `weaver status`, worktree
/// idle). Each gate is just state Claude records after a human answers once; we
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
///   when this launch actually runs with a bypass flag, since the dialog only
///   appears then.
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
    // Approve the ambient ANTHROPIC_API_KEY by its last 20 chars (how claude keys
    // these), when one is set and long enough to slice.
    let api_key_tail = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| k.len() >= 20)
        .map(|k| k[k.len() - 20..].to_string());
    // Workspace trust is recorded at the worktree's *main* repo root, which Claude
    // resolves a git worktree to, so trusting the root covers every worktree.
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
    /// Seed `bypassPermissionsModeAccepted` — only when launching with a bypass
    /// flag, since the dialog appears only then.
    bypass: bool,
    /// The ambient `ANTHROPIC_API_KEY`'s last 20 chars to mark approved, if set.
    api_key_tail: Option<&'a str>,
    /// The worktree's main repo root, to record workspace trust against.
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

// ---------------------------------------------------------------------------
// The one-shot headless agent
// ---------------------------------------------------------------------------

/// Markers of a *calling* Claude Code session, stripped before spawning a
/// subprocess so it runs fresh and isolated (the lint-review precedent).
/// Mirrors `scripts/lint-review.py`'s `STRIPPED_ENV`. Shared by the one-shot
/// agent here and the watch script executor.
pub const STRIPPED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
];

/// Spawn a one-shot headless agent: write `prompt` to its stdin, capture
/// stdout, strip the calling session's env markers. Best-effort: returns
/// `None` when the agent is absent, errors, or exceeds `timeout` — callers
/// must degrade gracefully, so a missing `claude` never breaks them.
///
/// The command is `WEAVER_WATCH_AGENT_CMD` (default `claude -p`); a
/// non-empty `model`/`effort` is appended as `--model`/`--effort`.
pub async fn run_oneshot(
    prompt: &str,
    model: &str,
    effort: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    let cmd_str =
        std::env::var("WEAVER_WATCH_AGENT_CMD").unwrap_or_else(|_| "claude -p".to_string());
    let mut parts = cmd_str.split_whitespace();
    let program = parts.next()?;
    let mut args: Vec<String> = parts.map(str::to_string).collect();
    if !model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(model.trim().to_string());
    }
    if !effort.trim().is_empty() {
        args.push("--effort".to_string());
        args.push(effort.trim().to_string());
    }

    let mut command = tokio::process::Command::new(program);
    command
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for key in STRIPPED_ENV {
        command.env_remove(key);
    }

    let mut child = command.spawn().ok()?; // agent not on PATH → None, caller degrades.
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(prompt.as_bytes()).await;
        // Drop stdin so the agent sees EOF and proceeds.
        drop(stdin);
    }

    let out = tokio::time::timeout(timeout, child.wait_with_output()).await;
    match out {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_ctx<'a>(
        goal_file: Option<&'a Path>,
        primer_file: Option<&'a Path>,
        model: &'a str,
        effort: &'a str,
    ) -> AgentLaunchContext<'a> {
        AgentLaunchContext {
            branch_id: "",
            work_dir: Path::new("."),
            term_session: "",
            goal_file,
            primer_file,
            server_addr: "",
            model,
            effort,
            extra_env: &[],
        }
    }

    /// Build a full launch script for a builtin runtime the way [`start_terminal`]
    /// does — its inner command wrapped with the env exports — without spawning a
    /// terminal, so the command strings can be asserted directly. `runtime`
    /// `"shell"` means a bare login shell (no inner command).
    fn launch_script(
        runtime: &str,
        goal_file: Option<&Path>,
        primer_file: Option<&Path>,
        env: &[(&str, &str)],
        mode: LaunchMode,
        model: &str,
        effort: &str,
    ) -> String {
        let ctx = test_ctx(goal_file, primer_file, model, effort);
        let inner = match runtime {
            "claude" => claude_command(&ctx, mode),
            "codex" => codex_command(&ctx),
            "shell" => String::new(),
            other => panic!("unexpected runtime in test helper: {other}"),
        };
        wrap_launch_script(&inner, env, None)
    }

    #[test]
    fn bare_shell_script_just_execs_a_shell() {
        assert_eq!(bare_shell_script(&[], None), "exec \"${SHELL:-/bin/sh}\"");
        // The `"shell"` runtime in the test helper builds the same bare shell.
        let script = launch_script("shell", None, None, &[], LaunchMode::Fresh, "", "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    fn custom_agent(name: &str, setup: &str, launch: &str, resume: &str) -> CustomAgent {
        CustomAgent {
            name: name.to_string(),
            label: name.to_string(),
            setup: setup.to_string(),
            launch: launch.to_string(),
            resume: resume.to_string(),
            reports_status: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn custom_agent_runs_setup_then_launch_with_the_goal() {
        let ctx = test_ctx(Some(Path::new("/x/goal.txt")), None, "", "");
        let a = custom_agent("aider", "printf hooks > .cfg", "aider --message", "");
        // Fresh: setup, then the launch command with the goal content appended.
        assert_eq!(
            custom_command(&a, &ctx, LaunchMode::Fresh),
            "printf hooks > .cfg; aider --message \"$(cat '/x/goal.txt')\""
        );
    }

    #[test]
    fn custom_agent_adopt_prefers_resume_and_drops_the_goal() {
        let ctx = test_ctx(Some(Path::new("/x/goal.txt")), None, "", "");
        // With a resume command, adopt runs it (setup first) and passes no goal.
        let a = custom_agent("aider", "setup.sh", "aider --message", "aider --continue");
        assert_eq!(
            custom_command(&a, &ctx, LaunchMode::Adopt),
            "setup.sh; aider --continue"
        );
        // Without a resume command, adopt falls back to launch-with-goal.
        let b = custom_agent("aider", "", "aider --message", "");
        assert_eq!(
            custom_command(&b, &ctx, LaunchMode::Adopt),
            "aider --message \"$(cat '/x/goal.txt')\""
        );
    }

    #[test]
    fn custom_agent_with_no_launch_command_execs_a_bare_shell() {
        // A setup-only (or wholly empty) custom agent produces an empty inner
        // command, so its session just execs the login shell — the role the old
        // builtin "shell" agent filled, now expressible as a custom agent.
        let ctx = test_ctx(Some(Path::new("/x/goal.txt")), None, "", "");
        let a = custom_agent("bare", "", "", "");
        assert_eq!(custom_command(&a, &ctx, LaunchMode::Fresh), "");
        assert_eq!(
            wrap_launch_script(&custom_command(&a, &ctx, LaunchMode::Fresh), &[], None),
            "exec \"${SHELL:-/bin/sh}\""
        );
    }

    #[test]
    fn claude_script_exports_env_and_runs_claude() {
        let script = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            None,
            &[("WEAVER_API", "http://h:1")],
            LaunchMode::Fresh,
            "",
            "",
        );
        assert!(script.contains("export WEAVER_API='http://h:1'; "));
        assert!(script.contains("claude \"$(cat '/x/goal.txt')\"; "));
        assert!(script.ends_with("exec \"${SHELL:-/bin/sh}\""));
    }

    #[test]
    fn concierge_primer_rides_in_as_system_prompt_not_a_positional() {
        // A concierge launches with its primer as appended system context and NO
        // positional prompt, so it boots primed but idle (takes no turn until the
        // operator speaks). Fresh and adopt both append the primer.
        let fresh = launch_script(
            "claude",
            None,
            Some(Path::new("/x/primer.txt")),
            &[],
            LaunchMode::Fresh,
            "",
            "",
        );
        assert_eq!(
            fresh,
            "claude --append-system-prompt-file '/x/primer.txt'; exec \"${SHELL:-/bin/sh}\""
        );
        // No positional `$(cat …)` prompt — that is what made it take a turn on boot.
        assert!(!fresh.contains("$(cat"), "got: {fresh}");

        // Adopt re-appends the primer (the system prompt is rebuilt per launch) and
        // resumes the conversation with --continue.
        let adopt = launch_script(
            "claude",
            None,
            Some(Path::new("/x/primer.txt")),
            &[],
            LaunchMode::Adopt,
            "",
            "",
        );
        assert_eq!(
            adopt,
            "claude --continue --append-system-prompt-file '/x/primer.txt'; \
             exec \"${SHELL:-/bin/sh}\""
        );
        assert!(!adopt.contains("$(cat"), "got: {adopt}");
    }

    #[test]
    fn concierge_kind_is_a_role_not_a_runtime() {
        // The `concierge` string marks a role; it is not a runtime, so it must be
        // resolved (to claude|codex) before launch and never reach the command
        // builder — it is not a builtin agent type.
        assert!(builtin_agent_type("claude").is_some());
        assert!(builtin_agent_type("codex").is_some());
        assert!(builtin_agent_type(CONCIERGE_KIND).is_none());
        // Hook-starting is a runtime capability, not a role property: only claude
        // fires weaver's lifecycle hooks.
        assert!(
            builtin_agent_type("claude")
                .unwrap()
                .metadata()
                .supports_hooks
        );
        assert!(
            !builtin_agent_type("codex")
                .unwrap()
                .metadata()
                .supports_hooks
        );
        // The primer the concierge is seeded with is the real fleet-ops doc.
        assert!(concierge_primer().contains("fleet concierge"));
    }

    #[test]
    fn codex_runtime_runs_codex_with_its_prompt() {
        // A concierge resolved to the codex runtime launches `codex "$(cat …)"`,
        // seeding the primer as codex's opening prompt.
        let fresh = launch_script(
            "codex",
            Some(Path::new("/x/goal.txt")),
            None,
            &[],
            LaunchMode::Fresh,
            "",
            "",
        );
        assert!(
            fresh.contains("codex \"$(cat '/x/goal.txt')\"; "),
            "got: {fresh}"
        );
        // Codex has no scoped resume, so adopt re-launches fresh with the primer.
        let adopt = launch_script(
            "codex",
            Some(Path::new("/x/goal.txt")),
            None,
            &[],
            LaunchMode::Adopt,
            "",
            "",
        );
        assert!(
            adopt.contains("codex \"$(cat '/x/goal.txt')\"; "),
            "got: {adopt}"
        );
    }

    #[test]
    fn codex_concierge_still_seeds_the_primer_positionally() {
        // Codex has no `--append-system-prompt-file`, so a codex concierge (primer
        // in `primer_file`, no `goal_file`) falls back to seeding the primer as a
        // positional prompt — it still takes a turn on boot. Making codex boot idle
        // is a documented follow-up.
        let fresh = launch_script(
            "codex",
            None,
            Some(Path::new("/x/primer.txt")),
            &[],
            LaunchMode::Fresh,
            "",
            "",
        );
        assert!(
            fresh.contains("codex \"$(cat '/x/primer.txt')\"; "),
            "got: {fresh}"
        );
    }

    #[test]
    fn env_values_with_single_quotes_are_escaped() {
        // An operator env var whose value contains a single quote must not break
        // out of the `export NAME='…'` assignment.
        let script = launch_script(
            "shell",
            None,
            None,
            &[("MSG", "it's \"quoted\"")],
            LaunchMode::Fresh,
            "",
            "",
        );
        assert!(
            script.contains(r#"export MSG='it'\''s "quoted"'; "#),
            "got: {script}"
        );
    }

    #[test]
    fn effort_and_model_args() {
        assert_eq!(effort_args("xhigh"), "--effort xhigh");
        assert_eq!(effort_args(""), "");
        assert_eq!(model_args("opus"), "--model opus");
        assert_eq!(model_args("fable"), "--model fable");
        assert_eq!(model_args(""), "");
    }

    #[test]
    fn combine_args_layers_model_and_effort() {
        assert_eq!(combine_args("opus", "high"), "--model opus --effort high");
        assert_eq!(combine_args("", "max"), "--effort max");
        assert_eq!(combine_args("haiku", ""), "--model haiku");
        assert_eq!(combine_args("", ""), "");
    }

    #[test]
    fn codex_protocol_maps_tiers_and_effort_to_codex_flags() {
        let script = launch_script(
            "codex",
            Some(Path::new("/x/goal.txt")),
            None,
            &[],
            LaunchMode::Fresh,
            "gpt-5.5",
            "xhigh",
        );
        assert!(
            script.contains("codex --model gpt-5.5 -c model_reasoning_effort=\\\"xhigh\\\""),
            "got: {script}"
        );
    }

    #[test]
    fn adopt_mode_resumes_claude_with_continue() {
        let script = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            None,
            &[],
            LaunchMode::Adopt,
            "",
            "",
        );
        assert_eq!(script, "claude --continue; exec \"${SHELL:-/bin/sh}\"");
    }

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
        // A corrupt/unexpected shape is reset rather than panicking.
        let mut root = json!("not an object");
        assert!(apply_launch_gates(&mut root, &seed(false, None, None)));
        assert!(root.is_object());
        assert_eq!(root["hasCompletedOnboarding"], json!(true));
    }
}
