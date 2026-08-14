//! Agent registry and launch management for terminal sessions, ACP sessions,
//! and fresh ACP one-shot judgement calls.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::acp::{AcpLaunch, NewOrLoad};
use crate::backend;
use crate::custom_agents::CustomAgent;
use crate::db::Db;
use weaver_core::agent::{hooks_json, HookMode};
use weaver_core::BoxFut;

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
    /// True for the code-shipped `claude`/`codex`; false for an operator-defined
    /// custom agent (which the UI may edit or delete).
    pub builtin: bool,
    /// Whether this runtime can be driven through ACP. Kept separate from its
    /// declared/default protocol so callers test a capability, not a default.
    pub supports_acp: bool,
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

/// Derive picker/resolver metadata from an already-read custom-agent row.
///
/// Launch resolution keeps this metadata coupled to the exact command snapshot
/// it later executes instead of re-reading the mutable registry in between.
pub fn custom_metadata(agent: &CustomAgent) -> AgentMetadata {
    CustomAgentType::new(agent.clone()).metadata()
}

const MODEL_CHOICES: &[(&str, &str)] = &[
    ("haiku", "Haiku"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("fable", "Fable"),
];

const CODEX_MODEL_CHOICES: &[(&str, &str)] = &[
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4 Mini"),
    ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
];

pub use crate::agent_kind::{
    auto_approves_permissions, BuiltinAgentKind, CODEX_AGENT_MODE, DEFAULT_ACP_MODE,
};

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
    match BuiltinAgentKind::parse(kind)? {
        BuiltinAgentKind::Claude => Some(&CLAUDE_AGENT_TYPE),
        BuiltinAgentKind::Codex => Some(&CODEX_AGENT_TYPE),
    }
}

/// The builtin agents' metadata, in picker order.
pub fn builtin_metadata() -> Vec<AgentMetadata> {
    [&CLAUDE_AGENT_TYPE as &dyn AgentType, &CODEX_AGENT_TYPE]
        .into_iter()
        .map(AgentType::metadata)
        .collect()
}

#[derive(Debug, Deserialize)]
struct CodexModelCatalog {
    #[serde(default)]
    models: Vec<CodexCatalogModel>,
}

#[derive(Debug, Deserialize)]
struct CodexCatalogModel {
    slug: String,
    display_name: String,
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningLevel>,
}

#[derive(Debug, Deserialize)]
struct CodexReasoningLevel {
    effort: String,
}

/// Ask the installed Codex binary for its bundled model catalogue. This is a
/// stateless local query (`--bundled` forbids a network refresh), so Settings
/// reflects the version actually installed on the loom host without launching
/// a throwaway agent session. Older CLIs without `debug models` and malformed
/// catalogues simply retain the code-shipped fallback above.
async fn refresh_codex_metadata(metadata: &mut AgentMetadata) {
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new("codex")
            .args(["debug", "models", "--bundled"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(output)) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    apply_codex_catalog(metadata, &output.stdout);
}

fn apply_codex_catalog(metadata: &mut AgentMetadata, bytes: &[u8]) {
    let Ok(catalog) = serde_json::from_slice::<CodexModelCatalog>(bytes) else {
        return;
    };

    let mut models = Vec::new();
    let mut efforts = Vec::new();
    let mut seen_models = HashSet::new();
    let mut seen_efforts = HashSet::new();
    for model in catalog.models {
        if model.visibility != "list" || !seen_models.insert(model.slug.clone()) {
            continue;
        }
        models.push(AgentChoice {
            id: model.slug,
            label: model.display_name,
        });
        for level in model.supported_reasoning_levels {
            if seen_efforts.insert(level.effort.clone()) {
                efforts.push(AgentChoice {
                    label: effort_label(&level.effort),
                    id: level.effort,
                });
            }
        }
    }
    if !models.is_empty() {
        metadata.models = models;
    }
    if !efforts.is_empty() {
        metadata.efforts = efforts;
    }
}

fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "X-High".to_string(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
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
    if let Some(codex) = out.iter_mut().find(|meta| meta.kind == "codex") {
        refresh_codex_metadata(codex).await;
    }
    for a in crate::custom_agents::list(db).await? {
        out.push(CustomAgentType::new(a).metadata());
    }
    Ok(out)
}

/// The metadata for one agent kind, or `None` when it names no agent.
pub async fn metadata_for(db: &Db, kind: &str) -> Result<Option<AgentMetadata>> {
    let Some(resolved) = resolve(db, kind).await? else {
        return Ok(None);
    };
    let mut metadata = resolved.as_type().metadata();
    if metadata.kind == "codex" {
        refresh_codex_metadata(&mut metadata).await;
    }
    Ok(Some(metadata))
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

/// Check that `model` is one of `metadata`'s offered choices, or any explicit
/// model name when the agent accepts raw values. Blank always means the agent's
/// own default. A key-free reason on mismatch.
pub fn validate_model(metadata: &AgentMetadata, model: &str) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(());
    }
    if metadata.accepts_raw_model || metadata.models.iter().any(|choice| choice.id == model) {
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
        Some(f) => format!(
            "codex --disable apps{args} \"$(cat {})\"",
            sh_single_quote_path(f)
        ),
        None => format!("codex --disable apps{args}"),
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
    /// Prompt prelude policy stamped onto the session (`weaver` or `none`).
    pub prelude: &'a str,
    pub server_addr: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// Operator-managed environment variables exported into the session.
    pub extra_env: &'a [(String, String)],
    pub env_clear: bool,
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
            accepts_raw_model: true,
            supports_hooks: true,
            builtin: true,
            supports_acp: true,
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
            builtin: true,
            supports_acp: true,
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
            builtin: false,
            supports_acp: self.agent.protocol == "acp",
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
    /// The runtime to launch — a builtin (`claude`/`codex`) or a custom agent's
    /// name.
    pub runtime: &'a str,
    pub work_dir: &'a Path,
    pub term_session: &'a str,
    /// The **positional** opening prompt catted in as the operator's first
    /// message.
    pub goal_file: Option<&'a Path>,
    /// Optional system context appended via `--append-system-prompt-file` for
    /// runtimes that support it.
    pub primer_file: Option<&'a Path>,
    /// Prompt prelude policy stamped onto the session (`weaver` or `none`).
    pub prelude: &'a str,
    pub server_addr: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// Operator-managed environment variables ([`crate::agent_env`]) exported
    /// into the session on top of loom's own `WEAVER_*` / `LOOM_TOKEN`. The
    /// caller reads these from the database; an empty slice adds nothing.
    pub extra_env: &'a [(String, String)],
    pub env_clear: bool,
    /// Exact custom-agent definition accepted by canonical resolution. `None`
    /// resolves a builtin or retains the legacy/adopt behavior of consulting
    /// the current registry.
    pub custom: Option<&'a CustomAgent>,
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
        prelude: spec.prelude,
        server_addr: spec.server_addr,
        model: spec.model,
        effort: spec.effort,
        extra_env: spec.extra_env,
        env_clear: spec.env_clear,
        memory_max_gb: backend::memory_max_gb(db).await,
    };
    let resolved = if let Some(custom) = spec.custom {
        if custom.name != spec.runtime {
            return Err(anyhow!(
                "resolved custom agent '{}' does not match runtime '{}'",
                custom.name,
                spec.runtime
            ));
        }
        ResolvedAgent::Custom(CustomAgentType::new(custom.clone()))
    } else {
        resolve(db, spec.runtime)
            .await?
            .ok_or_else(|| anyhow!("unknown agent '{}'", spec.runtime))?
    };
    let agent_type = resolved.as_type();
    let _instance = match mode {
        LaunchMode::Fresh => agent_type.create(ctx).await,
        LaunchMode::Adopt => agent_type.adopt(ctx).await,
    }?;
    Ok(())
}

async fn prepare_claude(ctx: &AgentLaunchContext<'_>) {
    let weaver_bin = weaver_bin_path();

    if ctx.prelude == "weaver" {
        if let Err(e) = install_hooks(ctx.work_dir, &weaver_bin, HookMode::Terminal).await {
            tracing::warn!(work_dir = %ctx.work_dir.display(), error = %e,
                "agent hook setup failed; launching without lifecycle hooks");
        }
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
    // The session environment loom injects (WEAVER_API/WEAVER_BRANCH, session
    // credentials, and operator vars) — the same set the ACP relay launch
    // delivers (see [`session_env`] / [`build_acp_launch`]).
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
        ctx.env_clear,
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
    std::fs::read_to_string(crate::paths::local_token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The session environment loom injects into an agent process — the same set for
/// the terminal (PTY) and ACP (relay) backends. `extra_env` carries the freshly
/// minted session-bound `LOOM_TOKEN` and `LOOM_SESSION_ID`; the machine-local
/// admin token is never injected into an agent.
pub fn session_env(
    server_addr: &str,
    branch_id: &str,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("WEAVER_API".to_string(), format!("http://{server_addr}")),
        ("WEAVER_BRANCH".to_string(), branch_id.to_string()),
    ];
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
// then brings up. See `docs/ARCHITECTURE.md` and `docs/mcp-profiles.md`.
// ---------------------------------------------------------------------------

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
    /// Durable Loom session id, exposed only to the session-scoped MCP bridge.
    pub session_id: &'a str,
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
    pub env_clear: bool,
    /// The launch permission posture (`bypassPermissions`, `acceptEdits`, …).
    pub mode: &'a str,
    /// Whether Loom installs its standard Weaver orientation hook.
    pub prelude: &'a str,
    /// Stamped restricted-profile posture. Restricted sessions do not load
    /// Claude settings and any unmatched permission request is denied by Loom.
    pub restricted: bool,
    /// JSON array stamped on the session/profile.
    pub allowed_tools: &'a str,
    /// Provider-neutral MCP policy snapshot stamped onto the session.
    pub mcp_access: &'a str,
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
/// `DEFAULT_AUTH_REQUEST`). Codex's `agent` mode routes approval requests to
/// Loom, which answers one-shot grants by policy rather than relying on Codex's
/// model reviewer. Codex is hookless, so the primer rides the opening prompt
/// exactly as it does on the terminal path. For a custom acp agent it runs the
/// agent's `launch` command verbatim (its setup stage first) as the adapter, with
/// no `_meta`.
pub async fn build_acp_launch(
    db: &Db,
    spec: &AcpLaunchSpec<'_>,
    open: AcpOpen,
) -> Result<AcpLaunch> {
    let is_fresh = matches!(open, AcpOpen::Fresh);
    let is_codex = spec.custom.is_none() && spec.runtime == "codex";
    let is_claude = spec.custom.is_none() && !is_codex;
    let adapter_cmd = match spec.custom {
        // A custom acp agent's `launch` command *is* the adapter (its setup stage
        // runs first, as for a terminal custom agent).
        Some(agent) => join_shell(&[agent.setup.trim(), agent.launch.trim()]),
        None if is_codex => codex_acp_cmd(db).await,
        None => claude_acp_cmd(db).await,
    };

    if is_claude && spec.prelude == "weaver" && !spec.restricted {
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
    let allowed_tools: Vec<String> = serde_json::from_str(spec.allowed_tools)
        .context("invalid session runtime-permission snapshot")?;
    let mcp_snapshot: weaver_api::McpPolicySnapshot =
        serde_json::from_str(spec.mcp_access).context("invalid session MCP policy snapshot")?;
    if is_codex && goal_text.is_none() {
        // No appendSystemPrompt analogue: a primer-only launch seeds the primer
        // positionally, mirroring the terminal path's
        // `goal_file.or(primer_file)`.
        goal_text = primer_text.clone();
    }
    let meta = if is_claude {
        claude_acp_meta(
            spec.model,
            primer_text.as_deref(),
            spec.mode,
            spec.restricted,
            spec.allowed_tools,
        )
    } else {
        None
    };

    let mut env = session_env(spec.server_addr, spec.branch_id, spec.extra_env);
    push_env_default(&mut env, "LOOM_SESSION_ID", spec.session_id);
    if spec.restricted {
        // GitHub mutations are performed by Loom's server-side restricted tool
        // endpoint. The adapter and model never receive the credential.
        env.retain(|(name, _)| !matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN"));
        env.retain(|(name, _)| name != "CLAUDE_CODE_DISABLE_AUTO_MEMORY");
        env.push((
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string(),
            "1".to_string(),
        ));
    }
    if is_codex {
        // Adapter-contract env, deferring to any operator-provided value.
        push_env_default(
            &mut env,
            "DEFAULT_AUTH_REQUEST",
            r#"{"methodId":"api-key"}"#,
        );
        let codex_mode = codex_acp_mode(spec.mode);
        configure_codex_acp(&mut env, spec.model, spec.effort, &codex_mode)?;
        push_env_default(&mut env, "INITIAL_AGENT_MODE", &codex_mode);
    }
    let mcp_servers = crate::mcp::acp_server_configs(&allowed_tools, Some(&mcp_snapshot), &env);

    let (new_or_load, goal) = match open {
        AcpOpen::Fresh => (
            NewOrLoad::New {
                cwd: spec.work_dir.to_path_buf(),
                meta,
            },
            goal_text.filter(|g| !g.trim().is_empty()),
        ),
        // A load replays history — no goal, but restricted adapter metadata is
        // restated so tool/settings policy survives a process restart.
        AcpOpen::Load(id) => (
            NewOrLoad::Load {
                acp_session_id: id,
                meta,
            },
            None,
        ),
    };

    Ok(AcpLaunch {
        adapter_cmd,
        cwd: spec.work_dir.to_path_buf(),
        env,
        env_clear: spec.env_clear,
        mcp_servers,
        new_or_load,
        // Codex boots directly in its mapped mode via `INITIAL_AGENT_MODE`; a
        // post-setup `session/set_mode` would re-send a claude-flavored id it
        // does not advertise.
        mode: (!is_codex).then(|| spec.mode.to_string()),
        // Loading must preserve adapter-restored live choices: the user may
        // have changed either selector after launch.
        initial_model: (is_fresh && !spec.model.trim().is_empty())
            .then(|| spec.model.trim().to_string()),
        initial_effort: (is_fresh && !spec.effort.trim().is_empty())
            .then(|| spec.effort.trim().to_string()),
        goal,
        setup_timeout: std::time::Duration::from_secs(30),
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

/// Apply Loom's Codex policy while preserving the shared provider login and
/// operator-supplied adapter configuration.
///
/// codex-acp's `agent` mode supplies the workspace-write sandbox and on-request
/// approval policy. Loom must receive those requests in order to apply its
/// deterministic one-shot approval policy, so the default reviewer is `user`
/// (the ACP client) rather than Codex's model reviewer. An explicit reviewer in
/// `CODEX_CONFIG` wins over that default.
fn configure_codex_acp(
    env: &mut Vec<(String, String)>,
    model: &str,
    effort: &str,
    codex_mode: &str,
) -> Result<()> {
    let mut config = match env
        .iter()
        .rev()
        .find_map(|(name, value)| (name == "CODEX_CONFIG").then_some(value))
    {
        Some(value) => serde_json::from_str::<Value>(value).context("invalid CODEX_CONFIG JSON")?,
        None => Value::Object(codex_acp_config(model, effort)),
    };
    let config = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("CODEX_CONFIG must be a JSON object"))?;
    if codex_mode.trim() == CODEX_AGENT_MODE {
        config
            .entry("approvals_reviewer".to_string())
            .or_insert_with(|| json!("user"));
    }
    let features = config
        .entry("features".to_string())
        .or_insert_with(|| json!({}));
    let features = features
        .as_object_mut()
        .ok_or_else(|| anyhow!("CODEX_CONFIG.features must be a JSON object"))?;
    features.insert("apps".to_string(), json!(false));

    env.retain(|(name, _)| name != "CODEX_CONFIG");
    env.push((
        "CODEX_CONFIG".to_string(),
        Value::Object(config.clone()).to_string(),
    ));
    Ok(())
}

fn codex_acp_config(model: &str, effort: &str) -> Map<String, Value> {
    let mut cfg = Map::new();
    let model = model.trim();
    if !model.is_empty() {
        cfg.insert("model".to_string(), json!(model));
    }
    if let Some(e) = effort_flag(effort) {
        cfg.insert("model_reasoning_effort".to_string(), json!(e));
    }
    cfg
}

/// Map the launch mode onto a codex-acp `INITIAL_AGENT_MODE` id. The create API
/// speaks the claude-flavored vocabulary; codex's own ids pass through, so an
/// operator can name `read-only`/`agent`/`agent-full-access` directly.
fn codex_acp_mode(mode: &str) -> String {
    match mode.trim() {
        "bypassPermissions" => "agent-full-access".to_string(),
        // codex-acp has no distinct auto-approve mode. `agent` supplies the
        // workspace sandbox; Loom answers its one-shot approval requests.
        "acceptEdits" | "default" | "auto" | "" => CODEX_AGENT_MODE.to_string(),
        "plan" => "read-only".to_string(),
        other => other.to_string(),
    }
}

/// The `_meta.claudeCode.options` object for the claude adapter — only the fields
/// that are actually configured (model, the primer as `appendSystemPrompt`, the
/// permission mode). `None` when nothing is set.
fn claude_acp_meta(
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
// Transient ACP prompts
// ---------------------------------------------------------------------------

/// Best-effort digest generated through the incoming provider's ACP adapter.
pub struct HandoffSummary {
    pub text: Option<String>,
    pub model: Option<String>,
    pub status: &'static str,
}

enum OneShotPolicy<'a> {
    Profile {
        model: &'a str,
        effort: &'a str,
        profile: Option<&'a crate::profile::Profile>,
    },
    Metadata,
}

struct OneShotLaunchPolicy<'a> {
    extra_env: Vec<(String, String)>,
    env_clear: bool,
    mode: &'a str,
    prelude: &'a str,
    restricted: bool,
    allowed_tools: String,
    mcp_access: String,
    profile_model: &'a str,
    profile_effort: &'a str,
}

/// Central agent-resolution and non-interactive prompt surface. Interactive
/// launches and transient prompts both resolve the same registered runtime;
/// provider-specific execution remains behind that runtime's ACP adapter.
pub struct AgentManager<'a> {
    db: &'a Db,
    acp: &'a crate::acp::AcpRegistry,
}

impl<'a> AgentManager<'a> {
    pub fn new(db: &'a Db, acp: &'a crate::acp::AcpRegistry) -> Self {
        Self { db, acp }
    }

    /// Ask the incoming runtime's advertised Haiku/Luna-class model for a
    /// digest over a transient ACP session. The actual incoming launch is
    /// cloned for adapter/config parity, then stripped of session authority and
    /// MCP access. Any adapter, model-selection, timeout, or output failure
    /// degrades to the handoff fallback.
    pub async fn summarize_handoff(
        &self,
        runtime: &str,
        prompt: &str,
        incoming: &AcpLaunch,
        timeout: Duration,
    ) -> HandoffSummary {
        let metadata = match metadata_for(self.db, runtime).await {
            Ok(Some(metadata)) if metadata.supports_acp => metadata,
            Ok(_) => {
                return HandoffSummary {
                    text: None,
                    model: None,
                    status: "unavailable",
                };
            }
            Err(error) => {
                tracing::warn!(runtime, %error, "failed to resolve handoff summarizer");
                return HandoffSummary {
                    text: None,
                    model: None,
                    status: "unavailable",
                };
            }
        };
        let launch = transient_prompt_launch(incoming);
        match crate::acp::prompt_once(
            self.db,
            self.acp.transient_sessions(),
            launch,
            prompt,
            crate::acp::AcpPromptModel::FirstContaining(&["haiku", "luna"]),
            crate::acp::AcpPromptEffort::Prefer("low"),
            timeout,
        )
        .await
        {
            Ok(Some(output)) => HandoffSummary {
                text: Some(output.text),
                model: output.model,
                status: "generated",
            },
            Ok(None) => HandoffSummary {
                text: None,
                model: None,
                status: "unavailable",
            },
            Err(error) => {
                tracing::warn!(
                    runtime = %metadata.kind,
                    %error,
                    "incoming ACP handoff summary unavailable"
                );
                HandoffSummary {
                    text: None,
                    model: None,
                    status: "unavailable",
                }
            }
        }
    }

    /// Run the public judgement-call primitive through a fresh ACP session.
    /// Empty selectors retain the adapter's advertised defaults; explicit
    /// selectors must appear in the live ACP `configOptions`.
    pub async fn run_oneshot(
        &self,
        runtime: &str,
        prompt: &str,
        model: &str,
        effort: &str,
        profile: Option<&crate::profile::Profile>,
        timeout: Duration,
    ) -> Option<String> {
        self.run_oneshot_with(
            runtime,
            prompt,
            OneShotPolicy::Profile {
                model,
                effort,
                profile,
            },
            timeout,
        )
        .await
    }

    /// Run one bounded metadata prompt through the session's own ACP runtime.
    /// The transient launch has no ambient environment, session authority, MCP,
    /// or tools and accepts exactly one prompt. Only an advertised economy-class
    /// model is used; otherwise metadata assistance degrades to unavailable.
    pub async fn run_metadata(
        &self,
        runtime: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Option<String> {
        self.run_oneshot_with(runtime, prompt, OneShotPolicy::Metadata, timeout)
            .await
    }

    /// Boxed to keep this state machine's codegen in `loom-agent` — see the
    /// note on `loom_launch::provision::create`.
    fn run_oneshot_with<'f>(
        &'f self,
        runtime: &'f str,
        prompt: &'f str,
        policy: OneShotPolicy<'f>,
        timeout: Duration,
    ) -> BoxFut<'f, Option<String>> {
        Box::pin(self.run_oneshot_with_inner(runtime, prompt, policy, timeout))
    }

    async fn run_oneshot_with_inner(
        &self,
        runtime: &str,
        prompt: &str,
        policy: OneShotPolicy<'_>,
        timeout: Duration,
    ) -> Option<String> {
        let (model, effort, profile, economy) = match policy {
            OneShotPolicy::Profile {
                model,
                effort,
                profile,
            } => (model, effort, profile, false),
            OneShotPolicy::Metadata => ("", "", None, true),
        };
        match metadata_for(self.db, runtime).await {
            Ok(Some(metadata)) if metadata.supports_acp => {}
            Ok(_) => return None,
            Err(error) => {
                tracing::warn!(runtime, %error, "failed to resolve one-shot ACP runtime");
                return None;
            }
        }
        let custom = if builtin_agent_type(runtime).is_some() {
            None
        } else {
            match crate::custom_agents::get(self.db, runtime).await {
                Ok(Some(custom)) => Some(custom),
                Ok(None) => {
                    tracing::warn!(runtime, "one-shot ACP runtime disappeared");
                    return None;
                }
                Err(error) => {
                    tracing::warn!(runtime, %error, "failed to load one-shot ACP runtime");
                    return None;
                }
            }
        };
        let work_dir = match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(runtime, %error, "failed to resolve one-shot ACP working directory");
                return None;
            }
        };
        let launch_policy = match profile {
            Some(profile) => {
                let mut extra_env = match crate::profile::env_pairs(self.db, &profile.name).await {
                    Ok(env) => env,
                    Err(error) => {
                        tracing::warn!(runtime, profile = %profile.name, %error, "failed to resolve one-shot profile environment");
                        return None;
                    }
                };
                if profile.env_clear {
                    let allowlist = match profile.ambient_names() {
                        Ok(names) => names,
                        Err(error) => {
                            tracing::warn!(runtime, profile = %profile.name, %error, "failed to resolve one-shot profile ambient allowlist");
                            return None;
                        }
                    };
                    extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
                }
                let allowed_tools = match profile.mcp_policy_snapshot().and_then(|snapshot| {
                    crate::mcp::effective_allowed_tool_rules_for(profile, &snapshot)
                }) {
                    Ok(rules) => match serde_json::to_string(&rules) {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(runtime, profile = %profile.name, %error, "failed to serialize one-shot profile tools");
                            return None;
                        }
                    },
                    Err(error) => {
                        tracing::warn!(runtime, profile = %profile.name, %error, "failed to resolve one-shot profile tools");
                        return None;
                    }
                };
                OneShotLaunchPolicy {
                    extra_env,
                    env_clear: profile.env_clear,
                    mode: profile.mode.as_str(),
                    prelude: profile.prelude.as_str(),
                    restricted: profile.restricted,
                    allowed_tools,
                    mcp_access: profile.mcp_policy.clone(),
                    profile_model: profile.model.as_str(),
                    profile_effort: profile.effort.as_str(),
                }
            }
            None => {
                let mcp_access = match serde_json::to_string(
                    &weaver_api::McpPolicySnapshot::default(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(runtime, %error, "failed to build empty one-shot MCP policy");
                        return None;
                    }
                };
                OneShotLaunchPolicy {
                    // An env-cleared launch still needs the same non-secret
                    // process baseline as an env-cleared profile: in
                    // particular PATH must survive so a configured adapter
                    // command such as `node …` can actually be resolved.
                    extra_env: crate::profile::cleared_environment(Vec::new(), &[]),
                    env_clear: true,
                    mode: "plan",
                    prelude: "none",
                    restricted: true,
                    allowed_tools: "[]".to_string(),
                    mcp_access,
                    profile_model: "",
                    profile_effort: "",
                }
            }
        };
        let launch = match build_acp_launch(
            self.db,
            &AcpLaunchSpec {
                session_id: "oneshot",
                branch_id: "oneshot",
                runtime,
                work_dir: &work_dir,
                server_addr: "127.0.0.1:0",
                model: "",
                effort: "",
                goal_file: None,
                primer_file: None,
                extra_env: &launch_policy.extra_env,
                env_clear: launch_policy.env_clear,
                mode: launch_policy.mode,
                prelude: launch_policy.prelude,
                restricted: launch_policy.restricted,
                allowed_tools: &launch_policy.allowed_tools,
                mcp_access: &launch_policy.mcp_access,
                custom: custom.as_ref(),
            },
            AcpOpen::Fresh,
        )
        .await
        {
            Ok(launch) => transient_prompt_launch(&launch),
            Err(error) => {
                tracing::warn!(runtime, %error, "failed to build one-shot ACP launch");
                return None;
            }
        };
        let selected_model = if model.trim().is_empty() {
            launch_policy.profile_model
        } else {
            model.trim()
        };
        let selected_effort = if effort.trim().is_empty() {
            launch_policy.profile_effort
        } else {
            effort.trim()
        };
        const ECONOMY_MODEL_HINTS: &[&str] = &["haiku", "luna", "mini", "nano"];
        let preferred_model = if economy {
            crate::acp::AcpPromptModel::FirstContaining(ECONOMY_MODEL_HINTS)
        } else if selected_model.is_empty() {
            crate::acp::AcpPromptModel::Default
        } else {
            crate::acp::AcpPromptModel::Exact(selected_model)
        };
        let preferred_effort = if economy {
            crate::acp::AcpPromptEffort::Prefer("low")
        } else if selected_effort.is_empty() {
            crate::acp::AcpPromptEffort::Default
        } else {
            crate::acp::AcpPromptEffort::Exact(selected_effort)
        };
        match crate::acp::prompt_once(
            self.db,
            self.acp.transient_sessions(),
            launch,
            prompt,
            preferred_model,
            preferred_effort,
            timeout,
        )
        .await
        {
            Ok(Some(output)) => Some(output.text),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(runtime, %error, "one-shot ACP prompt unavailable");
                None
            }
        }
    }
}

fn transient_prompt_launch(incoming: &AcpLaunch) -> AcpLaunch {
    let mut launch = incoming.clone();
    let mut env = BTreeMap::new();
    if !launch.env_clear {
        env.extend(std::env::vars());
    }
    env.extend(launch.env.clone());
    env.retain(|name, _| {
        !name.starts_with("WEAVER_")
            && !name.starts_with("LOOM_")
            && !matches!(
                name.as_str(),
                "GH_TOKEN"
                    | "GITHUB_TOKEN"
                    | "CLAUDECODE"
                    | "CLAUDE_CODE_ENTRYPOINT"
                    | "CLAUDE_CODE_EXECPATH"
                    | "CLAUDE_CODE_SESSION_ID"
                    | "CLAUDE_CODE_SSE_PORT"
                    | "CODEX_CONFIG"
                    | "INITIAL_AGENT_MODE"
            )
    });
    launch.env = env.into_iter().collect();
    launch.env_clear = true;
    launch.mcp_servers.clear();
    launch.goal = None;
    launch.mode = Some("plan".to_string());
    launch.initial_model = None;
    launch.initial_effort = None;
    if let NewOrLoad::New {
        meta: Some(meta), ..
    } = &mut launch.new_or_load
    {
        if let Some(options) = meta
            .get_mut("claudeCode")
            .and_then(|claude| claude.get_mut("options"))
            .and_then(Value::as_object_mut)
        {
            options.remove("model");
            options.remove("appendSystemPrompt");
            options.insert("permissionMode".to_string(), json!("plan"));
            options.insert("allowedTools".to_string(), json!([]));
            options.insert("tools".to_string(), json!([]));
            options.insert("settingSources".to_string(), json!([]));
            options.insert("strictMcpConfig".to_string(), json!(true));
        }
    }
    launch
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
            prelude: "weaver",
            server_addr: "",
            model,
            effort,
            extra_env: &[],
            env_clear: false,
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

    #[test]
    fn codex_catalog_replaces_stale_fallback_choices() {
        let mut metadata = CODEX_AGENT_TYPE.metadata();
        apply_codex_catalog(
            &mut metadata,
            br#"{"models":[
                {"slug":"gpt-next","display_name":"GPT Next","visibility":"list",
                 "supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},
                {"slug":"hidden","display_name":"Hidden","visibility":"hide",
                 "supported_reasoning_levels":[{"effort":"medium"}]}
            ]}"#,
        );
        assert_eq!(
            metadata
                .models
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-next"]
        );
        assert_eq!(
            metadata
                .efforts
                .iter()
                .map(|choice| (choice.id.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("low", "Low"), ("ultra", "Ultra")]
        );
    }

    #[test]
    fn claude_accepts_versioned_model_names() {
        let metadata = CLAUDE_AGENT_TYPE.metadata();

        assert!(validate_model(&metadata, "claude-opus-4-8").is_ok());
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
    fn claude_primer_rides_in_as_system_prompt_not_a_positional() {
        // A primer launches as appended system context and not as a positional
        // prompt. Fresh and adopt both append it.
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
    fn codex_runtime_runs_codex_with_its_prompt() {
        // Codex launches with the goal as its opening prompt.
        let fresh = launch_script(
            "codex",
            Some(Path::new("/x/goal.txt")),
            None,
            LaunchMode::Fresh,
            "",
            "",
        );
        // This rendered shell command is the terminal runtime contract.
        assert_eq!(
            fresh,
            "codex --disable apps \"$(cat '/x/goal.txt')\"; exec \"${SHELL:-/bin/sh}\""
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
        assert_eq!(
            adopt,
            "codex --disable apps \"$(cat '/x/goal.txt')\"; exec \"${SHELL:-/bin/sh}\""
        );
    }

    #[test]
    fn codex_primer_is_seeded_positionally() {
        // Codex has no `--append-system-prompt-file`, so a primer with no goal
        // falls back to a positional prompt.
        let fresh = launch_script(
            "codex",
            None,
            Some(Path::new("/x/primer.txt")),
            LaunchMode::Fresh,
            "",
            "",
        );
        assert_eq!(
            fresh,
            "codex --disable apps \"$(cat '/x/primer.txt')\"; exec \"${SHELL:-/bin/sh}\""
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
    fn transient_acp_prompt_keeps_the_adapter_but_drops_session_authority() {
        let incoming = AcpLaunch {
            adapter_cmd: "configured-acp-adapter".to_string(),
            cwd: PathBuf::from("/worktree"),
            env: vec![
                ("ANTHROPIC_API_KEY".to_string(), "provider".to_string()),
                ("LOOM_TOKEN".to_string(), "session".to_string()),
                ("LOOM_SESSION_ID".to_string(), "session-1".to_string()),
                ("WEAVER_BRANCH".to_string(), "branch-1".to_string()),
                ("GH_TOKEN".to_string(), "github".to_string()),
            ],
            env_clear: false,
            mcp_servers: vec![json!({"name":"loom_history"})],
            new_or_load: NewOrLoad::New {
                cwd: PathBuf::from("/worktree"),
                meta: Some(json!({"provider":"options"})),
            },
            mode: Some("plan".to_string()),
            initial_model: Some("expensive".to_string()),
            initial_effort: Some("high".to_string()),
            goal: Some("real goal".to_string()),
            setup_timeout: Duration::from_secs(30),
        };

        let summary = transient_prompt_launch(&incoming);
        assert_eq!(summary.adapter_cmd, incoming.adapter_cmd);
        assert!(summary.env_clear);
        let NewOrLoad::New { cwd, meta } = summary.new_or_load else {
            panic!("transient prompt must remain a fresh ACP session");
        };
        assert_eq!(cwd, PathBuf::from("/worktree"));
        assert_eq!(meta, Some(json!({"provider":"options"})));
        assert!(summary
            .env
            .iter()
            .any(|(name, value)| name == "ANTHROPIC_API_KEY" && value == "provider"));
        for denied in ["LOOM_TOKEN", "LOOM_SESSION_ID", "WEAVER_BRANCH", "GH_TOKEN"] {
            assert!(!summary.env.iter().any(|(name, _)| name == denied));
        }
        assert!(summary.mcp_servers.is_empty());
        assert!(summary.goal.is_none());
        assert_eq!(summary.mode.as_deref(), Some("plan"));
        assert!(summary.initial_model.is_none());
        assert!(summary.initial_effort.is_none());
    }

    #[test]
    fn transient_claude_prompt_removes_live_session_options() {
        let incoming = AcpLaunch {
            adapter_cmd: "configured-acp-adapter".to_string(),
            cwd: PathBuf::from("/worktree"),
            env: vec![
                (
                    "CODEX_CONFIG".to_string(),
                    r#"{"model":"expensive"}"#.to_string(),
                ),
                ("INITIAL_AGENT_MODE".to_string(), "agent".to_string()),
                ("CLAUDECODE".to_string(), "1".to_string()),
            ],
            env_clear: true,
            mcp_servers: vec![json!({"name":"github"})],
            new_or_load: NewOrLoad::New {
                cwd: PathBuf::from("/worktree"),
                meta: Some(json!({
                    "claudeCode": {
                        "options": {
                            "model": "opus",
                            "appendSystemPrompt": "live primer",
                            "permissionMode": "bypassPermissions",
                            "allowedTools": ["Bash"],
                            "tools": ["Bash"],
                            "settingSources": ["user"]
                        }
                    }
                })),
            },
            mode: Some("bypassPermissions".to_string()),
            initial_model: Some("opus".to_string()),
            initial_effort: Some("high".to_string()),
            goal: Some("live goal".to_string()),
            setup_timeout: Duration::from_secs(30),
        };

        let transient = transient_prompt_launch(&incoming);
        assert!(transient.env.iter().all(|(name, _)| !matches!(
            name.as_str(),
            "CODEX_CONFIG" | "INITIAL_AGENT_MODE" | "CLAUDECODE"
        )));
        let NewOrLoad::New {
            meta: Some(meta), ..
        } = transient.new_or_load
        else {
            panic!("transient prompt must retain restricted adapter metadata");
        };
        assert_eq!(
            meta["claudeCode"]["options"],
            json!({
                "permissionMode": "plan",
                "allowedTools": [],
                "tools": [],
                "settingSources": [],
                "strictMcpConfig": true
            })
        );
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
        assert_eq!(
            script,
            "codex --disable apps --model gpt-5.5 -c model_reasoning_effort=\\\"xhigh\\\" \
             \"$(cat '/x/goal.txt')\"; exec \"${SHELL:-/bin/sh}\""
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
            builtin,
            supports_acp: builtin || protocol == "acp",
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
        assert!(codex_acp_config("", "").is_empty());

        let cfg = codex_acp_config("gpt-5.3-codex", "high");
        assert_eq!(cfg["model"], "gpt-5.3-codex");
        assert_eq!(cfg["model_reasoning_effort"], "high");

        let model_only = codex_acp_config("gpt-5.3-codex", " ");
        assert_eq!(model_only["model"], "gpt-5.3-codex");
        assert!(model_only.get("model_reasoning_effort").is_none());
    }

    #[test]
    fn codex_acp_agent_mode_routes_approvals_to_loom() {
        let mut env = vec![(
            "CODEX_CONFIG".to_string(),
            r#"{"model":"operator","features":{"shell_snapshot":true,"apps":true}}"#.to_string(),
        )];

        configure_codex_acp(&mut env, "ignored", "high", "agent").unwrap();

        let config: Value = serde_json::from_str(
            env.iter()
                .find_map(|(name, value)| (name == "CODEX_CONFIG").then_some(value))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(config["model"], "operator");
        assert_eq!(config["approvals_reviewer"], "user");
        assert_eq!(config["features"]["shell_snapshot"], true);
        assert_eq!(config["features"]["apps"], false);
    }

    #[test]
    fn codex_acp_configuration_preserves_explicit_reviewer() {
        let mut explicit_reviewer = vec![(
            "CODEX_CONFIG".to_string(),
            r#"{"approvals_reviewer":"auto_review"}"#.to_string(),
        )];
        configure_codex_acp(&mut explicit_reviewer, "", "", "agent").unwrap();
        let config: Value = serde_json::from_str(&explicit_reviewer[0].1).unwrap();
        assert_eq!(config["approvals_reviewer"], "auto_review");
    }

    #[test]
    fn codex_acp_only_agent_mode_sets_a_default_reviewer() {
        for mode in ["read-only", "agent-full-access"] {
            let mut env = Vec::new();
            configure_codex_acp(&mut env, "", "", mode).unwrap();
            let config: Value = serde_json::from_str(&env[0].1).unwrap();
            assert!(
                config.get("approvals_reviewer").is_none(),
                "{mode} must not set a default reviewer"
            );
        }
    }

    #[test]
    fn sessions_boot_in_auto_by_default() {
        // Every ACP session boots in the provider-neutral `auto` posture unless
        // overridden. Codex uses its workspace-write `agent` sandbox mode while
        // Loom owns its one-shot approval decisions.
        assert_eq!(DEFAULT_ACP_MODE, "auto");
        assert_eq!(codex_acp_mode(DEFAULT_ACP_MODE), "agent");
    }

    #[test]
    fn codex_acp_mode_maps_the_claude_vocabulary_and_passes_codex_ids_through() {
        assert_eq!(codex_acp_mode("bypassPermissions"), "agent-full-access");
        assert_eq!(codex_acp_mode("acceptEdits"), "agent");
        assert_eq!(codex_acp_mode("default"), "agent");
        // Loom-owned auto-approval is configured separately; the sandbox remains
        // `agent`.
        assert_eq!(codex_acp_mode("auto"), "agent");
        assert_eq!(codex_acp_mode(""), "agent");
        assert_eq!(codex_acp_mode("plan"), "read-only");
        // Codex's own ids are honoured verbatim.
        assert_eq!(codex_acp_mode("read-only"), "read-only");
        assert_eq!(codex_acp_mode("agent-full-access"), "agent-full-access");
    }

    #[test]
    fn auto_approve_modes_cover_full_access_and_codex_agent() {
        // The two explicit "never prompt me" postures and Codex's Loom-reviewed
        // Agent mode all take the deterministic one-shot approval path.
        assert!(auto_approves_permissions("bypassPermissions"));
        assert!(auto_approves_permissions("agent-full-access"));
        assert!(auto_approves_permissions("  agent-full-access  "));
        assert!(auto_approves_permissions("agent"));
        // codex full access round-trips through the launch mapping into the id
        // the gate recognizes.
        assert!(auto_approves_permissions(&codex_acp_mode(
            "bypassPermissions"
        )));
        // Everything else must still prompt.
        for mode in ["auto", "acceptEdits", "default", "plan", "read-only", ""] {
            assert!(
                !auto_approves_permissions(mode),
                "{mode} must not auto-approve"
            );
        }
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
        assert!(claude_acp_meta("", None, "", false, "[]").is_none());

        let m =
            claude_acp_meta("opus", Some("be careful"), "bypassPermissions", false, "[]").unwrap();
        let opts = &m["claudeCode"]["options"];
        assert_eq!(opts["model"], "opus");
        assert_eq!(opts["appendSystemPrompt"], "be careful");
        assert_eq!(opts["permissionMode"], "bypassPermissions");

        // A blank primer is dropped; model/mode still ride.
        let m2 = claude_acp_meta("sonnet", Some("  "), "plan", false, "[]").unwrap();
        let opts2 = &m2["claudeCode"]["options"];
        assert_eq!(opts2["model"], "sonnet");
        assert!(opts2.get("appendSystemPrompt").is_none());

        let restricted = claude_acp_meta(
            "",
            None,
            "default",
            true,
            r#"["Read(./**)","mcp__loom_github__issue_view","mcp__loom_github__issue_comment"]"#,
        )
        .unwrap();
        let restricted = &restricted["claudeCode"]["options"];
        assert_eq!(restricted["settingSources"], json!([]));
        assert_eq!(restricted["strictMcpConfig"], true);
        assert_eq!(restricted["tools"], json!(["Read", "Glob", "Grep"]));
        assert_eq!(
            restricted["allowedTools"],
            json!([
                "Read(./**)",
                "mcp__loom_github__issue_view",
                "mcp__loom_github__issue_comment"
            ])
        );
        assert!(restricted.get("mcpServers").is_none());
        assert_eq!(opts2["permissionMode"], "plan");
    }

    #[tokio::test]
    async fn restricted_acp_launch_keeps_github_token_out_of_the_adapter() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let extra_env = vec![
            ("GH_TOKEN".to_string(), "server-only".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "model-key".to_string()),
            ("LOOM_TOKEN".to_string(), "session-token".to_string()),
        ];
        let launch = build_acp_launch(
            &db,
            &AcpLaunchSpec {
                session_id: "session-1",
                branch_id: "branch-1",
                runtime: "claude",
                work_dir: work_dir.path(),
                server_addr: "127.0.0.1:7878",
                model: "",
                effort: "",
                goal_file: None,
                primer_file: None,
                extra_env: &extra_env,
                env_clear: true,
                mode: "default",
                prelude: "none",
                restricted: true,
                allowed_tools: r#"["Read(./**)","mcp__loom_github__issue_edit"]"#,
                mcp_access:
                    r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[],"custom_servers":[]}"#,
                custom: None,
            },
            AcpOpen::Fresh,
        )
        .await
        .unwrap();

        assert!(!launch
            .env
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN")));
        assert!(launch
            .env
            .iter()
            .any(|(name, value)| name == "ANTHROPIC_API_KEY" && value == "model-key"));
        assert!(launch
            .env
            .iter()
            .any(|(name, value)| name == "LOOM_SESSION_ID" && value == "session-1"));
        assert!(launch
            .env
            .iter()
            .any(|(name, value)| { name == "CLAUDE_CODE_DISABLE_AUTO_MEMORY" && value == "1" }));
        assert_eq!(launch.mcp_servers.len(), 1);
        assert_eq!(launch.mcp_servers[0]["name"], "loom_github");
        assert!(launch.mcp_servers[0]["command"]
            .as_str()
            .is_some_and(|command| std::path::Path::new(command).is_absolute()));
        assert_eq!(
            launch.mcp_servers[0]["args"],
            json!(["mcp", "serve", "github"])
        );
        assert_eq!(
            launch.mcp_servers[0]["env"],
            json!([
                {
                    "name": "LOOM_MCP_ALLOWED_TOOLS",
                    "value": "[\"issue_edit\"]"
                },
                {
                    "name": "WEAVER_API",
                    "value": "http://127.0.0.1:7878"
                },
                {
                    "name": "WEAVER_BRANCH",
                    "value": "branch-1"
                },
                {
                    "name": "LOOM_TOKEN",
                    "value": "session-token"
                },
                {
                    "name": "LOOM_SESSION_ID",
                    "value": "session-1"
                }
            ])
        );
    }
}
