//! Launching coding agents into per-session terminals, plus the **one-shot
//! headless agent** (`POST /api/agent/oneshot`) — a fresh, env-stripped agent
//! run for a judgement call.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::acp::{AcpLaunch, NewOrLoad};
use crate::backend;
use crate::custom_agents::CustomAgent;
use crate::db::Db;
use weaver_core::agent::{hooks_json, HookMode};

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
    /// The agent's declared execution backend: `"terminal"` or `"acp"`. The
    /// builtins declare `"acp"`; a custom agent reports its stored `protocol`.
    /// A create request may override a builtin's default (`--protocol
    /// terminal` keeps the PTY); see [`resolve_protocol`].
    pub protocol: String,
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

/// The lifecycle status a freshly launched or resumed session starts in. Every
/// runtime is live the moment its terminal spawns, so this is always `running` —
/// there is no separate `launching` state to wait out. Kept as the single place
/// that names a new session's initial status.
pub async fn initial_status(_db: &Db, _runtime: &str) -> &'static str {
    "running"
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
    /// Per-session memory ceiling in GiB (0 = unlimited), resolved from the
    /// `session.memory_max_gb` setting by [`launch`].
    pub memory_max_gb: u64,
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
            // ACP is the builtin default; `--protocol terminal` keeps the PTY
            // fallback.
            protocol: "acp".to_string(),
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
            // ACP is the builtin default; `--protocol terminal` keeps the PTY
            // fallback.
            protocol: "acp".to_string(),
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
            protocol: if self.agent.protocol.is_empty() {
                "terminal".to_string()
            } else {
                self.agent.protocol.clone()
            },
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

/// Wrap a value in single quotes for safe interpolation into the launch script —
/// e.g. a goal/primer file path in `"$(cat '…')"` — escaping any embedded single
/// quote the POSIX way (`'\''` — close the quote, an escaped literal quote,
/// reopen). Paths come from the operator's filesystem and are arbitrary, so a
/// stray `'` must not break out of the quotes. (Environment values are *not*
/// quoted here: they no longer ride in the script — see [`wrap_launch_script`].)
fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sh_single_quote_path(path: &Path) -> String {
    sh_single_quote(&path.display().to_string())
}

/// Build the launch script the supervisor runs as `sh -c <script>`: prepend the
/// weaver bin dir to `$PATH`, run the (optional) inner agent command, then `exec`
/// the login shell. The session's environment is delivered **out of band** (via
/// the process environment — see [`start_terminal`] / [`backend::new_session`]),
/// not `export`-ed here, so secret values never land on the child shell's argv.
/// The `$PATH` prepend stays in the script because it needs the shell to expand
/// the inherited `$PATH` at runtime; it carries no secret.
fn wrap_launch_script(inner: &str, weaver_dir: Option<&Path>) -> String {
    let mut script = String::new();
    if let Some(dir) = weaver_dir {
        script.push_str(&format!("export PATH=\"{}:$PATH\"; ", dir.display()));
    }
    if !inner.is_empty() {
        script.push_str(inner);
        script.push_str("; ");
    }
    script.push_str("exec \"${SHELL:-/bin/sh}\"");
    script
}

/// The launch script for a **bare login shell** — the `$PATH` prepend, then
/// `exec` the shell, with no inner agent command. Used by the operator scratch
/// shell and per-session debug shells ([`crate::shell`]); those are plain shells,
/// not agents, so they don't go through [`launch`] or an [`AgentType`]. Their
/// environment is delivered out of band alongside this script, same as an agent's.
pub fn bare_shell_script(weaver_dir: Option<&Path>) -> String {
    wrap_launch_script("", weaver_dir)
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
        memory_max_gb: backend::memory_max_gb(db).await,
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
    let weaver_bin = weaver_bin_path();

    if let Err(e) = install_hooks(ctx.work_dir, &weaver_bin, HookMode::Terminal).await {
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
    // The session environment loom injects (WEAVER_API/WEAVER_BRANCH/LOOM_TOKEN +
    // operator vars) — the same set the ACP relay launch delivers (see
    // [`session_env`] / [`build_acp_launch`]).
    let env_owned = session_env(ctx.server_addr, ctx.branch_id, ctx.extra_env);
    let env: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // The env is delivered to the child through the process environment (see
    // `backend::new_session` → `tapestry::spawn_detached`), not `export`-ed into
    // the script — so tokens/keys never appear on any process's argv.
    let script = wrap_launch_script(inner, weaver_dir);
    tracing::debug!(
        branch = ctx.branch_id,
        runtime,
        session = ctx.term_session,
        "launching agent session"
    );
    backend::new_session(
        ctx.term_session,
        ctx.work_dir,
        &script,
        &env,
        ctx.memory_max_gb,
    )
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

/// The session environment loom injects into an agent process — the same set for
/// the terminal (PTY) and ACP (relay) backends: `WEAVER_API`, `WEAVER_BRANCH`, the
/// machine-local `LOOM_TOKEN` (when minted), then the operator-managed vars last
/// (so a stored var wins any shared name — safe, since `agent_env::validate_name`
/// reserves loom's own `WEAVER_*`/`LOOM_` prefixes). Delivered out of band via the
/// process environment, never on argv.
pub fn session_env(
    server_addr: &str,
    branch_id: &str,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("WEAVER_API".to_string(), format!("http://{server_addr}")),
        ("WEAVER_BRANCH".to_string(), branch_id.to_string()),
    ];
    if let Some(token) = read_local_token() {
        env.push(("LOOM_TOKEN".to_string(), token));
    }
    for (k, v) in extra_env {
        env.push((k.clone(), v.clone()));
    }
    env
}

/// The `weaver` binary path — a sibling of the running executable (they ship
/// together), falling back to bare `weaver` on `PATH`.
fn weaver_bin_path() -> String {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(|d| d.join("weaver").display().to_string())
        .unwrap_or_else(|| "weaver".to_string())
}

// ---------------------------------------------------------------------------
// ACP launch mapping (protocol='acp' sessions)
//
// The ACP analogue of the terminal launch path: instead of building an argv the
// PTY runs, it builds an [`AcpLaunch`] the relay runs (adapter command + env +
// `_meta` options + the goal as the first prompt), which [`crate::acp::start`]
// then brings up. See docs/plans/acp.md "Launch mapping".
// ---------------------------------------------------------------------------

/// The permission posture an ACP session boots in when the create request names
/// none — the moral equivalent of the retired pre-seeded launch gates.
pub const DEFAULT_ACP_MODE: &str = "bypassPermissions";

/// Resolve the execution backend for a launch: the agent's declared `protocol`
/// unless the create request overrides it. A blank/absent override keeps the
/// declared value. Both builtins may opt into `acp` (claude via
/// `claude-agent-acp`, codex via `codex-acp`); a terminal-only custom agent has
/// no ACP adapter, so it is rejected. Returns a key-free reason.
pub fn resolve_protocol(meta: &AgentMetadata, requested: Option<&str>) -> Result<String, String> {
    let declared = if meta.protocol.is_empty() {
        "terminal"
    } else {
        meta.protocol.as_str()
    };
    let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(declared.to_string());
    };
    if req != "terminal" && req != "acp" {
        return Err(format!(
            "unknown protocol '{req}' (expected 'terminal' or 'acp')"
        ));
    }
    if req == declared {
        return Ok(declared.to_string());
    }
    if req == "acp" {
        // Opting into ACP: both builtins have an adapter; a terminal custom
        // agent does not.
        return if meta.builtin {
            Ok("acp".to_string())
        } else {
            Err(format!(
                "agent '{}' does not support the acp protocol",
                meta.kind
            ))
        };
    }
    // req == "terminal" while declared == "acp": force the terminal fallback. Only
    // a builtin has a terminal launch path; an acp custom agent has no terminal
    // command to run.
    if meta.builtin {
        Ok("terminal".to_string())
    } else {
        Err(format!(
            "agent '{}' is acp-only and has no terminal fallback",
            meta.kind
        ))
    }
}

/// How [`build_acp_launch`] opens the ACP session.
pub enum AcpOpen {
    /// A fresh `session/new`; the goal file's content seeds the first prompt.
    Fresh,
    /// Reload an existing agent session id (`session/load` — history replays, no
    /// new goal). Used by adopt when the adapter advertised `loadSession`.
    Load(String),
}

/// Everything [`build_acp_launch`] needs — the ACP analogue of [`LaunchSpec`],
/// carrying the same launch inputs but mapping them onto the adapter / `_meta` /
/// goal shape the protocol takes.
pub struct AcpLaunchSpec<'a> {
    pub branch_id: &'a str,
    /// The resolved runtime: the builtin `claude`, or a custom agent's name.
    pub runtime: &'a str,
    pub work_dir: &'a Path,
    pub server_addr: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// The positional opening prompt file (`goal.txt`) — its *content* becomes the
    /// first `session/prompt`. `None` boots the session idle.
    pub goal_file: Option<&'a Path>,
    /// The system-context file (`primer.txt`) — its content becomes the adapter's
    /// `appendSystemPrompt` option (the `--append-system-prompt-file` analogue).
    pub primer_file: Option<&'a Path>,
    pub extra_env: &'a [(String, String)],
    /// The launch permission posture (`bypassPermissions`, `acceptEdits`, …).
    pub mode: &'a str,
    /// The resolved custom agent when `runtime` names one (its `launch` command is
    /// the ACP adapter); `None` for the builtin claude.
    pub custom: Option<&'a CustomAgent>,
}

/// Build the [`AcpLaunch`] for a `protocol='acp'` session. For the builtin claude
/// this resolves the `claude-agent-acp` adapter command, installs the
/// SessionStart-only hook bundle (the work-cycle hooks and the launch-gate seed
/// are redundant under ACP), and maps model/primer/mode into `_meta.claudeCode.
/// options`. For the builtin codex it resolves the `codex-acp` adapter and maps
/// the same inputs onto its env contract (`CODEX_CONFIG`, `INITIAL_AGENT_MODE`,
/// `DEFAULT_AUTH_REQUEST`) — codex is hookless, so the primer rides the opening
/// prompt exactly as it does on the terminal path. For a custom acp agent it
/// runs the agent's `launch` command verbatim (its setup stage first) as the
/// adapter, with no `_meta`.
pub async fn build_acp_launch(
    db: &Db,
    spec: &AcpLaunchSpec<'_>,
    open: AcpOpen,
) -> Result<AcpLaunch> {
    let is_codex = spec.custom.is_none() && spec.runtime == "codex";
    let is_claude = spec.custom.is_none() && !is_codex;
    let adapter_cmd = match spec.custom {
        // A custom acp agent's `launch` command *is* the adapter (its setup stage
        // runs first, as for a terminal custom agent).
        Some(agent) => join_shell(&[agent.setup.trim(), agent.launch.trim()]),
        None if is_codex => codex_acp_cmd(db).await,
        None => claude_acp_cmd(db).await,
    };

    if is_claude {
        // Install only the SessionStart primer hook; the work-cycle hooks and the
        // launch-gate seed are redundant under ACP (protocol turn edges + the
        // bypass posture replace them).
        let weaver_bin = weaver_bin_path();
        if let Err(e) = install_hooks(spec.work_dir, &weaver_bin, HookMode::Acp).await {
            tracing::warn!(work_dir = %spec.work_dir.display(), error = %e,
                "acp hook setup failed; launching without the primer hook");
        }
    }

    let primer_text = read_opt(spec.primer_file).await;
    let mut goal_text = read_opt(spec.goal_file).await;
    if is_codex && goal_text.is_none() {
        // No appendSystemPrompt analogue: a primer-only launch (the concierge
        // shape) seeds the primer positionally, mirroring the terminal path's
        // `goal_file.or(primer_file)`.
        goal_text = primer_text.clone();
    }
    let meta = if is_claude {
        claude_acp_meta(spec.model, primer_text.as_deref(), spec.mode)
    } else {
        None
    };

    let mut env = session_env(spec.server_addr, spec.branch_id, spec.extra_env);
    if is_codex {
        // Adapter-contract env, deferring to any operator-provided value.
        push_env_default(
            &mut env,
            "DEFAULT_AUTH_REQUEST",
            r#"{"methodId":"api-key"}"#,
        );
        if let Some(cfg) = codex_acp_config(spec.model, spec.effort) {
            push_env_default(&mut env, "CODEX_CONFIG", &cfg);
        }
        push_env_default(&mut env, "INITIAL_AGENT_MODE", &codex_acp_mode(spec.mode));
    }

    let (new_or_load, goal) = match open {
        AcpOpen::Fresh => (
            NewOrLoad::New {
                cwd: spec.work_dir.to_path_buf(),
                meta,
            },
            goal_text.filter(|g| !g.trim().is_empty()),
        ),
        // A load replays history — no goal, and `_meta` is unused.
        AcpOpen::Load(id) => (NewOrLoad::Load { acp_session_id: id }, None),
    };

    Ok(AcpLaunch {
        adapter_cmd,
        cwd: spec.work_dir.to_path_buf(),
        env,
        new_or_load,
        // Codex boots directly in its mapped mode via `INITIAL_AGENT_MODE`; a
        // post-setup `session/set_mode` would re-send a claude-flavored id it
        // does not advertise.
        mode: (!is_codex).then(|| spec.mode.to_string()),
        goal,
    })
}

/// Append `(key, value)` unless `key` is already present (an operator override
/// via extra_env wins over the adapter-contract default).
fn push_env_default(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !env.iter().any(|(k, _)| k == key) {
        env.push((key.to_string(), value.to_string()));
    }
}

/// Where the claude CLI records its conversations (`~/.claude/projects`).
pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

/// The newest claude conversation recorded for `work_dir` under `projects_dir`:
/// claude munges the cwd into a directory name (every non-alphanumeric byte
/// becomes `-`) holding one `<session-id>.jsonl` per conversation. These are
/// the sessions `claude --continue` resumes, and the ACP adapter loads the same
/// ids — which is what lets an orphaned terminal session adopt into ACP over
/// its own history. `None` when nothing is recorded for that directory.
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

/// The default command for an npm-distributed ACP adapter: the installed bin
/// when present (the deploy pins exact versions onto PATH), else `npx` fetching
/// the package at launch (the dev-machine path).
fn npm_adapter_cmd(bin: &str, package: &str) -> String {
    format!("command -v {bin} >/dev/null 2>&1 && exec {bin}; exec npx --yes {package}")
}

/// The `claude-agent-acp` adapter command: `WEAVER_CLAUDE_ACP_CMD` (env) wins,
/// then the `acp.claude_cmd` setting, else the npm default.
async fn claude_acp_cmd(db: &Db) -> String {
    if let Ok(cmd) = std::env::var("WEAVER_CLAUDE_ACP_CMD") {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            return cmd.to_string();
        }
    }
    if let Some(cmd) = weaver_core::config::get(db, "acp.claude_cmd")
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return cmd;
    }
    npm_adapter_cmd("claude-agent-acp", "@agentclientprotocol/claude-agent-acp")
}

/// The `codex-acp` adapter command: `WEAVER_CODEX_ACP_CMD` (env) wins, then the
/// `acp.codex_cmd` setting, else the npm default (the package bundles a
/// compatible `@openai/codex`).
async fn codex_acp_cmd(db: &Db) -> String {
    if let Ok(cmd) = std::env::var("WEAVER_CODEX_ACP_CMD") {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            return cmd.to_string();
        }
    }
    if let Some(cmd) = weaver_core::config::get(db, "acp.codex_cmd")
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return cmd;
    }
    npm_adapter_cmd("codex-acp", "@agentclientprotocol/codex-acp")
}

/// The `CODEX_CONFIG` JSON for the codex adapter (merged into the Codex session
/// config): model and reasoning effort, only when set. `None` when neither is.
fn codex_acp_config(model: &str, effort: &str) -> Option<String> {
    let mut cfg = Map::new();
    let model = model.trim();
    if !model.is_empty() {
        cfg.insert("model".to_string(), json!(model));
    }
    if let Some(e) = effort_flag(effort) {
        cfg.insert("model_reasoning_effort".to_string(), json!(e));
    }
    if cfg.is_empty() {
        return None;
    }
    Some(Value::Object(cfg).to_string())
}

/// Map the launch mode onto a codex-acp `INITIAL_AGENT_MODE` id. The create API
/// speaks the claude-flavored vocabulary; codex's own ids pass through, so an
/// operator can name `read-only`/`agent`/`agent-full-access` directly.
fn codex_acp_mode(mode: &str) -> String {
    match mode.trim() {
        "bypassPermissions" => "agent-full-access".to_string(),
        "acceptEdits" | "default" | "" => "agent".to_string(),
        "plan" => "read-only".to_string(),
        other => other.to_string(),
    }
}

/// The `_meta.claudeCode.options` object for the claude adapter — only the fields
/// that are actually configured (model, the primer as `appendSystemPrompt`, the
/// permission mode). `None` when nothing is set.
fn claude_acp_meta(model: &str, primer: Option<&str>, mode: &str) -> Option<Value> {
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
    if options.is_empty() {
        return None;
    }
    Some(json!({ "claudeCode": { "options": options } }))
}

/// Read a file's content, or `None` when the path is absent or unreadable.
async fn read_opt(path: Option<&Path>) -> Option<String> {
    match path {
        Some(p) => tokio::fs::read_to_string(p).await.ok(),
        None => None,
    }
}

/// Write (merging into any existing file) `.claude/settings.local.json` so the
/// agent reports status to weaver via hooks. `mode` selects the bundle: a
/// terminal session installs the full working/idle set; an ACP session installs
/// only `SessionStart` (its turn edges come from the protocol — see
/// [`weaver_core::agent::HookMode`]).
pub async fn install_hooks(work_dir: &Path, weaver_bin: &str, mode: HookMode) -> Result<()> {
    let dir = work_dir.join(".claude");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("settings.local.json");
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    let hooks = hooks_json(weaver_bin, mode);
    root["hooks"] = hooks["hooks"].clone();
    tokio::fs::write(&path, serde_json::to_string_pretty(&root)?).await?;
    tracing::debug!(path = %path.display(), ?mode, "claude hooks installed");
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
///
/// `WEAVER_BRANCH` is stripped for a subtler reason than the rest. A nested
/// `claude -p` still reads the worktree's `.claude/settings.local.json` and
/// fires the weaver lifecycle hooks (`SessionStart`/`Stop`/…) — verified against
/// a real `claude`; `--settings '{"hooks":{}}'` does *not* suppress them. Left in
/// the child's env, `$WEAVER_BRANCH` makes each hook write an `idle`/`working`
/// event attributed to the *parent* branch, mid-turn, corrupting the very signal
/// the dashboard and `loom session wait` key on. Stripping it makes `weaver hook`
/// a no-op (it has no branch to key on). These agents are pipe-in/pipe-out — they
/// never call the `weaver` CLI — so they lose nothing by not carrying it.
pub const STRIPPED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "WEAVER_BRANCH",
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
            memory_max_gb: 0,
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
        wrap_launch_script(&inner, None)
    }

    #[test]
    fn bare_shell_script_just_execs_a_shell() {
        assert_eq!(bare_shell_script(None), "exec \"${SHELL:-/bin/sh}\"");
        // The `"shell"` runtime in the test helper builds the same bare shell.
        let script = launch_script("shell", None, None, LaunchMode::Fresh, "", "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    /// A nested headless agent must not carry `$WEAVER_BRANCH`, or it fires the
    /// worktree's weaver lifecycle hooks against the parent branch (see the
    /// constant's own docs). Guard the strip so it can't silently regress.
    #[test]
    fn stripped_env_drops_the_branch_marker() {
        assert!(
            STRIPPED_ENV.contains(&"WEAVER_BRANCH"),
            "nested agents must not inherit $WEAVER_BRANCH: {STRIPPED_ENV:?}"
        );
    }

    fn custom_agent(name: &str, setup: &str, launch: &str, resume: &str) -> CustomAgent {
        CustomAgent {
            name: name.to_string(),
            label: name.to_string(),
            setup: setup.to_string(),
            launch: launch.to_string(),
            resume: resume.to_string(),
            reports_status: false,
            protocol: "terminal".to_string(),
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
            wrap_launch_script(&custom_command(&a, &ctx, LaunchMode::Fresh), None),
            "exec \"${SHELL:-/bin/sh}\""
        );
    }

    #[test]
    fn claude_script_runs_claude_and_keeps_env_off_the_script() {
        let script = launch_script(
            "claude",
            Some(Path::new("/x/goal.txt")),
            None,
            LaunchMode::Fresh,
            "",
            "",
        );
        assert!(script.contains("claude \"$(cat '/x/goal.txt')\"; "));
        assert!(script.ends_with("exec \"${SHELL:-/bin/sh}\""));
        // Regression guard: the session environment is delivered out of band via
        // the process environment (see `start_terminal` / `backend::new_session`),
        // so no `export` of an env var may leak into the argv-visible script.
        assert!(
            !script.contains("export WEAVER_API") && !script.contains("export LOOM_TOKEN"),
            "env must not be baked into the launch script: {script}"
        );
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
    fn operator_env_never_reaches_the_launch_script() {
        // Operator env vars used to be shell-quoted into the script as
        // `export NAME='…'`; they are now delivered via the process environment
        // (`CommandBuilder::env`, off argv) instead. A value with shell
        // metacharacters therefore needs no quoting and — crucially — must not
        // appear in the script at all, so `ps` can't read it. The script is a
        // pure function of the inner command, independent of the env.
        let script = launch_script("shell", None, None, LaunchMode::Fresh, "", "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
        assert!(!script.contains("export MSG"), "got: {script}");
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

    fn meta_for(kind: &str, protocol: &str, builtin: bool) -> AgentMetadata {
        AgentMetadata {
            kind: kind.to_string(),
            label: kind.to_string(),
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: false,
            supports_hooks: false,
            supports_concierge: false,
            builtin,
            protocol: protocol.to_string(),
        }
    }

    #[test]
    fn resolve_protocol_honours_declared_and_overrides() {
        let claude = meta_for("claude", "terminal", true);
        let codex = meta_for("codex", "terminal", true);
        let acp_custom = meta_for("my-acp", "acp", false);
        let term_custom = meta_for("aider", "terminal", false);

        // No override → declared.
        assert_eq!(resolve_protocol(&claude, None).unwrap(), "terminal");
        assert_eq!(resolve_protocol(&acp_custom, None).unwrap(), "acp");
        assert_eq!(resolve_protocol(&claude, Some("")).unwrap(), "terminal");

        // claude opts into acp; forcing terminal on it is a no-op.
        assert_eq!(resolve_protocol(&claude, Some("acp")).unwrap(), "acp");
        assert_eq!(
            resolve_protocol(&claude, Some("terminal")).unwrap(),
            "terminal"
        );

        // codex opts into acp via codex-acp.
        assert_eq!(resolve_protocol(&codex, Some("acp")).unwrap(), "acp");

        // A terminal-only custom agent has no acp adapter.
        assert!(resolve_protocol(&term_custom, Some("acp")).is_err());
        // An acp-only custom agent has no terminal fallback.
        assert!(resolve_protocol(&acp_custom, Some("terminal")).is_err());

        // An unknown protocol name is rejected.
        assert!(resolve_protocol(&claude, Some("grpc")).is_err());
    }

    #[test]
    fn latest_claude_session_id_picks_the_newest_recorded_conversation() {
        let projects = tempfile::tempdir().unwrap();
        let work_dir = Path::new("/w/repo/.worktrees/fix_things");
        // claude's munge: every non-alphanumeric byte becomes '-'.
        let munged = projects.path().join("-w-repo--worktrees-fix-things");
        std::fs::create_dir_all(&munged).unwrap();

        assert_eq!(
            latest_claude_session_id(projects.path(), work_dir),
            None,
            "an empty project dir records no conversation"
        );
        assert_eq!(
            latest_claude_session_id(projects.path(), Path::new("/elsewhere")),
            None,
            "a directory claude never saw records nothing"
        );

        std::fs::write(munged.join("older.jsonl"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(munged.join("newer.jsonl"), "{}").unwrap();
        std::fs::write(munged.join("not-a-session.txt"), "").unwrap();
        assert_eq!(
            latest_claude_session_id(projects.path(), work_dir).as_deref(),
            Some("newer")
        );
    }

    #[test]
    fn codex_acp_config_carries_only_configured_fields() {
        assert!(codex_acp_config("", "").is_none());

        let cfg: Value =
            serde_json::from_str(&codex_acp_config("gpt-5.3-codex", "high").unwrap()).unwrap();
        assert_eq!(cfg["model"], "gpt-5.3-codex");
        assert_eq!(cfg["model_reasoning_effort"], "high");

        let model_only: Value =
            serde_json::from_str(&codex_acp_config("gpt-5.3-codex", " ").unwrap()).unwrap();
        assert_eq!(model_only["model"], "gpt-5.3-codex");
        assert!(model_only.get("model_reasoning_effort").is_none());
    }

    #[test]
    fn codex_acp_mode_maps_the_claude_vocabulary_and_passes_codex_ids_through() {
        assert_eq!(codex_acp_mode("bypassPermissions"), "agent-full-access");
        assert_eq!(codex_acp_mode("acceptEdits"), "agent");
        assert_eq!(codex_acp_mode("default"), "agent");
        assert_eq!(codex_acp_mode(""), "agent");
        assert_eq!(codex_acp_mode("plan"), "read-only");
        // Codex's own ids are honoured verbatim.
        assert_eq!(codex_acp_mode("read-only"), "read-only");
        assert_eq!(codex_acp_mode("agent-full-access"), "agent-full-access");
    }

    #[test]
    fn push_env_default_defers_to_an_existing_key() {
        let mut env = vec![(
            "CODEX_CONFIG".to_string(),
            "{\"model\":\"mine\"}".to_string(),
        )];
        push_env_default(&mut env, "CODEX_CONFIG", "{\"model\":\"ours\"}");
        push_env_default(&mut env, "INITIAL_AGENT_MODE", "agent");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].1, "{\"model\":\"mine\"}");
        assert_eq!(
            env[1],
            ("INITIAL_AGENT_MODE".to_string(), "agent".to_string())
        );
    }

    #[test]
    fn claude_acp_meta_sets_only_configured_fields() {
        // Nothing configured → no _meta at all.
        assert!(claude_acp_meta("", None, "").is_none());

        let m = claude_acp_meta("opus", Some("be careful"), "bypassPermissions").unwrap();
        let opts = &m["claudeCode"]["options"];
        assert_eq!(opts["model"], "opus");
        assert_eq!(opts["appendSystemPrompt"], "be careful");
        assert_eq!(opts["permissionMode"], "bypassPermissions");

        // A blank primer is dropped; model/mode still ride.
        let m2 = claude_acp_meta("sonnet", Some("  "), "plan").unwrap();
        let opts2 = &m2["claudeCode"]["options"];
        assert_eq!(opts2["model"], "sonnet");
        assert!(opts2.get("appendSystemPrompt").is_none());
        assert_eq!(opts2["permissionMode"], "plan");
    }
}
