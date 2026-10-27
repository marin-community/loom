//! Agent registry and launch management for terminal sessions, ACP sessions,
//! and fresh ACP one-shot judgement calls.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::acp::{AcpLaunch, NewOrLoad};
use crate::backend;
pub use crate::catalog::model_successor;
use crate::custom_agents::CustomAgent;
use crate::db::Db;
use weaver_core::agent::{hooks_json, HookMode};
use weaver_core::BoxFut;

#[derive(Debug, Clone, Serialize)]
pub struct AgentChoice {
    pub id: String,
    pub label: String,
    /// For a model choice: the effort levels valid for *this* model, in the
    /// order the harness presents them. Empty for an effort choice, and for a
    /// model whose harness does not scope effort per model or whose options
    /// have not been fetched (the picker then uses the agent's global list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<AgentChoice>,
    /// Canonical fast choices for this model (`true` / `false`). Empty when
    /// the model has no fast option or its options have not been fetched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fast: Vec<AgentChoice>,
}

impl AgentChoice {
    pub(crate) fn leaf(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            efforts: Vec::new(),
            fast: Vec::new(),
        }
    }

    /// Materialize a builtin's static `(id, label)` table into the owned choices
    /// the metadata carries.
    fn list(pairs: &[(&str, &str)]) -> Vec<AgentChoice> {
        pairs
            .iter()
            .map(|(id, label)| Self::leaf(*id, *label))
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
    /// Whether the agent binary is available on the system PATH. Used by the
    /// UI to hide unavailable agent harnesses (e.g. when `codex` is not
    /// installed).
    pub available: Option<bool>,
}

pub struct AgentInstance {
    pub term_session: String,
}

pub type AgentFuture<'a> = Pin<Box<dyn Future<Output = Result<AgentInstance>> + Send + 'a>>;

pub trait AgentType: Sync {
    fn metadata(&self) -> AgentMetadata;

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a>;
    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a>;
}

pub struct ClaudeAgentType;
pub struct CodexAgentType;
pub struct OpenCodeAgentType;
pub struct CursorAgentType;

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
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
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
static OPENCODE_AGENT_TYPE: OpenCodeAgentType = OpenCodeAgentType;
static CURSOR_AGENT_TYPE: CursorAgentType = CursorAgentType;

/// The `AgentType` for a builtin kind. Exhaustive, so a new [`BuiltinAgentKind`]
/// variant forces an arm here.
fn builtin_agent_type_for(kind: BuiltinAgentKind) -> &'static dyn AgentType {
    match kind {
        BuiltinAgentKind::Claude => &CLAUDE_AGENT_TYPE,
        BuiltinAgentKind::Codex => &CODEX_AGENT_TYPE,
        BuiltinAgentKind::OpenCode => &OPENCODE_AGENT_TYPE,
        BuiltinAgentKind::CursorAgent => &CURSOR_AGENT_TYPE,
    }
}

/// The builtin agent for `kind`, if it names one. Custom agents live in the
/// database and are resolved by [`resolve`]; this covers only the code-shipped
/// runtimes, which need no DB lookup.
pub fn builtin_agent_type(kind: &str) -> Option<&'static dyn AgentType> {
    Some(builtin_agent_type_for(BuiltinAgentKind::parse(kind)?))
}

/// The builtin agents' metadata, in picker order.
pub fn builtin_metadata() -> Vec<AgentMetadata> {
    BuiltinAgentKind::ALL
        .into_iter()
        .map(|kind| builtin_agent_type_for(kind).metadata())
        .collect()
}

pub async fn is_claude_available() -> bool {
    binary_on_path("claude").await
}

pub async fn is_codex_available() -> bool {
    binary_on_path("codex").await
}

pub async fn is_opencode_available() -> bool {
    binary_on_path("opencode").await
}

pub async fn is_cursor_agent_available() -> bool {
    cursor_agent_bin().await.is_some()
}

/// How to invoke Cursor's CLI agent: the Homebrew `cursor-agent` binary when
/// present, else the `cursor agent` subcommand. `None` when neither is on
/// `PATH`.
async fn cursor_agent_bin() -> Option<&'static str> {
    if binary_on_path("cursor-agent").await {
        Some("cursor-agent")
    } else if binary_on_path("cursor").await {
        Some("cursor agent")
    } else {
        None
    }
}

/// Whether `bin` resolves to an executable file on `PATH`.
///
/// Reports presence, not health: a binary that exists but misbehaves still
/// counts as available. On Windows every `PATHEXT` suffix is tried, since an
/// npm-installed CLI ships as `bin.cmd`/`bin.ps1` and never `bin.exe`;
/// elsewhere the file must be a regular file with an execute bit. The
/// filesystem walk runs on a blocking thread so a slow `PATH` entry cannot
/// stall the async runtime.
async fn binary_on_path(bin: &str) -> bool {
    let bin = bin.to_string();
    tokio::task::spawn_blocking(move || {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        let names = candidate_names(&bin);
        std::env::split_paths(&path)
            .any(|dir| names.iter().any(|name| is_executable_file(&dir.join(name))))
    })
    .await
    .unwrap_or(false)
}

#[cfg(windows)]
fn candidate_names(bin: &str) -> Vec<String> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut names = vec![bin.to_string()];
    for ext in exts.split(';').filter(|ext| !ext.is_empty()) {
        names.push(format!("{bin}{}", ext.to_ascii_lowercase()));
    }
    names
}

#[cfg(not(windows))]
fn candidate_names(bin: &str) -> [String; 1] {
    [bin.to_string()]
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(windows))]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
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
    let mut out = Vec::new();
    for mut meta in builtin_metadata() {
        let kind = BuiltinAgentKind::parse(&meta.kind);
        meta.available = Some(match kind {
            Some(kind) => builtin_available_kind(kind).await,
            None => true,
        });
        crate::catalog::apply_stored(db, &meta.kind, &mut meta.models, &mut meta.efforts).await;
        out.push(meta);
    }
    for a in crate::custom_agents::list(db).await? {
        let mut meta = CustomAgentType::new(a).metadata();
        // A custom agent is an operator-defined command, not a binary loom probes.
        meta.available = Some(true);
        out.push(meta);
    }
    Ok(out)
}

/// Whether a code-shipped harness binary is on `PATH`.
pub async fn builtin_available_kind(kind: BuiltinAgentKind) -> bool {
    match kind {
        BuiltinAgentKind::Claude => is_claude_available().await,
        BuiltinAgentKind::Codex => is_codex_available().await,
        BuiltinAgentKind::OpenCode => is_opencode_available().await,
        BuiltinAgentKind::CursorAgent => is_cursor_agent_available().await,
    }
}

/// The ACP adapter command for a builtin harness, using the same override
/// precedence as a live launch.
pub async fn acp_command_for(db: &Db, kind: &str) -> Option<String> {
    match BuiltinAgentKind::parse(kind)? {
        BuiltinAgentKind::Claude => Some(claude_acp_cmd(db).await),
        BuiltinAgentKind::Codex => Some(codex_acp_cmd(db).await),
        BuiltinAgentKind::OpenCode => Some(opencode_acp_cmd(db).await),
        BuiltinAgentKind::CursorAgent => Some(cursor_agent_acp_cmd(db).await),
    }
}
/// The metadata for one agent kind, or `None` when it names no agent.
pub async fn metadata_for(db: &Db, kind: &str) -> Result<Option<AgentMetadata>> {
    let Some(resolved) = resolve(db, kind).await? else {
        return Ok(None);
    };
    let metadata = resolved.as_type().metadata();
    // Leave the stored catalogue off this snapshot. `available` and the
    // catalogue both feed the resolver-revision hash, and a refresh or a
    // launch write-through must not invalidate a preview that already passed.
    Ok(Some(metadata))
}

/// Whether `kind` names a known agent (builtin or custom).
pub async fn exists(db: &Db, kind: &str) -> bool {
    matches!(metadata_for(db, kind).await, Ok(Some(_)))
}

/// The lifecycle status a freshly launched or resumed session starts in. Every
/// runtime is live the moment its terminal spawns, so this is always `running` —
/// there is no separate `launching` state to wait out.
pub async fn initial_status(_db: &Db, _runtime: &str) -> &'static str {
    "running"
}

/// Blank always means the agent's own default. A harness that accepts a raw
/// model id skips the list; the live session is the authority for those.
/// Everyone else, including a builtin with a fixed list, has to name a choice
/// the registry currently offers.
pub fn validate_model(metadata: &AgentMetadata, model: &str) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() || metadata.accepts_raw_model {
        return Ok(());
    }
    if metadata.models.iter().any(|choice| choice.id == model) {
        Ok(())
    } else {
        Err(format!("unknown model '{model}' for {}", metadata.kind))
    }
}

/// Blank selects the agent's own default. Builtin harnesses accept any
/// explicit effort: the live session rejects a value it does not advertise.
/// Custom agents are checked against their static list.
pub fn validate_effort(metadata: &AgentMetadata, _model: &str, effort: &str) -> Result<(), String> {
    let effort = effort.trim();
    if effort.is_empty() || metadata.builtin {
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

fn spaced(args: String) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" {args}")
    }
}

/// The Opencode TUI is the terminal fallback for the ACP builtin: `--continue`
/// resumes in an existing worktree, a fresh run preloads the goal via
/// `--prompt`.
fn opencode_command(ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = spaced(join_args([
        model_flag(ctx.model).map(|m| format!("--model {m}"))
    ]));
    match (mode, ctx.goal_file.or(ctx.primer_file)) {
        (LaunchMode::Adopt, _) => format!("opencode --continue{args}"),
        (LaunchMode::Fresh, Some(f)) => {
            format!(
                "opencode{args} --prompt \"$(cat {})\"",
                sh_single_quote_path(f)
            )
        }
        (LaunchMode::Fresh, None) => format!("opencode{args}"),
    }
}

/// Cursor's CLI as the terminal fallback: `--continue` resumes, a fresh run
/// takes the goal positionally. `--model` is passed through verbatim (Cursor
/// scopes effort inside the model id / bracket parameters).
fn cursor_agent_command(bin: &str, ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = spaced(join_args([
        model_flag(ctx.model).map(|m| format!("--model {m}"))
    ]));
    match (mode, ctx.goal_file.or(ctx.primer_file)) {
        (LaunchMode::Adopt, _) => format!("{bin} --continue{args}"),
        (LaunchMode::Fresh, Some(f)) => {
            format!("{bin}{args} \"$(cat {})\"", sh_single_quote_path(f))
        }
        (LaunchMode::Fresh, None) => format!("{bin}{args}"),
    }
}

/// The inner launch command for a custom agent: its `setup` stage (if any), then
/// the stage command for this `mode`. Fresh runs `launch` with the goal file
/// appended as a positional argument (mirroring the builtin runtimes); adopt runs
/// `resume` with no goal, falling back to `launch`-with-goal when `resume` is
/// blank. An empty result execs a bare shell — a setup-only or command-less agent.
fn custom_command(agent: &CustomAgent, ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let launch_with_goal = |cmd: &str| match ctx.goal_file.or(ctx.primer_file) {
        // Goal *content*, not the file path, rides positionally (as `claude
        // "$(cat …)"` does).
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
            protocol: "acp".to_string(),
            available: None,
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
            protocol: "acp".to_string(),
            available: None,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move { start_terminal(&ctx, "codex", &codex_command(&ctx)).await })
    }
}

impl AgentType for OpenCodeAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "opencode".to_string(),
            label: "Opencode".to_string(),
            // The picker reads models from the stored catalogue.
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: true,
            supports_hooks: false,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Fresh)).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Adopt)).await
        })
    }
}

impl AgentType for CursorAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "cursor-agent".to_string(),
            label: "Cursor".to_string(),
            // The picker reads models from the stored catalogue.
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: true,
            supports_hooks: false,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            let bin = cursor_agent_bin().await.unwrap_or("cursor-agent");
            start_terminal(
                &ctx,
                "cursor-agent",
                &cursor_agent_command(bin, &ctx, LaunchMode::Fresh),
            )
            .await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> AgentFuture<'a> {
        Box::pin(async move {
            let bin = cursor_agent_bin().await.unwrap_or("cursor-agent");
            start_terminal(
                &ctx,
                "cursor-agent",
                &cursor_agent_command(bin, &ctx, LaunchMode::Adopt),
            )
            .await
        })
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
            available: None,
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
/// stray `'` must not break out of the quotes. Environment values aren't
/// quoted here; they ride out of band — see [`wrap_launch_script`].
fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sh_single_quote_path(path: &Path) -> String {
    sh_single_quote(&path.display().to_string())
}

const GITHUB_CLI_ADAPTER_MARKER: &str = "# Loom GitHub CLI credential adapter";

fn is_loom_github_cli_adapter(path: &Path) -> bool {
    use std::io::Read;

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut prefix = String::new();
    file.take(512)
        .read_to_string(&mut prefix)
        .is_ok_and(|_| prefix.contains(GITHUB_CLI_ADAPTER_MARKER))
}

/// Resolve the stock GitHub CLI without selecting a per-session adapter from a
/// parent Loom session. A nested development server inherits its caller's
/// `PATH`, so blindly taking the first `gh` can make the child adapter recurse
/// through the parent's adapter.
fn stock_github_cli() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("gh"))
        .find(|candidate| {
            if !candidate.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate
                    .metadata()
                    .map(|metadata| metadata.permissions().mode() & 0o111 == 0)
                    .unwrap_or(true)
                {
                    return false;
                }
            }
            !is_loom_github_cli_adapter(candidate)
        })
}

/// Build the `gh` transport adapter installed ahead of the stock CLI in a
/// managed session's `PATH`. Brokered credentials are resolved for every
/// invocation, so a long-lived caller such as Marin's `wait_for.py` can poll
/// beyond GitHub's one-hour installation-token lifetime.
fn github_cli_adapter_script(loom_bin: &Path, github_cli: Option<&Path>) -> String {
    let loom_bin = sh_single_quote_path(loom_bin);
    let launch = github_cli.map_or_else(
        || "echo \"stock GitHub CLI executable not found\" >&2\nexit 127".to_string(),
        |path| format!("exec {} \"$@\"", sh_single_quote_path(path)),
    );
    format!(
        r#"#!/bin/sh
{GITHUB_CLI_ADAPTER_MARKER}
case "${{LOOM_GITHUB_AUTH_MODE:-}}" in
  broker)
    if [ -z "$LOOM_SESSION_ID" ] || [ -z "$LOOM_TOKEN" ]; then
      echo "Loom-managed GitHub auth is missing its session credential" >&2
      exit 1
    fi
    repository=
    repository_argument=
    for argument in "$@"; do
      if [ "$repository_argument" = next ]; then
        repository="$argument"
        break
      fi
      case "$argument" in
        --repo|-R) repository_argument=next ;;
        --repo=*) repository="${{argument#--repo=}}"; break ;;
        -R?*) repository="${{argument#-R}}"; break ;;
        --) break ;;
      esac
    done
    if [ -z "$repository" ]; then
      repository="${{GH_REPO:-}}"
    fi
    if [ -z "$repository" ]; then
      origin="$(git config --get remote.origin.url 2>/dev/null || true)"
      origin="${{origin%.git}}"
      case "$origin" in
        *github.com[:/]*) repository="${{origin##*github.com[:/]}}" ;;
      esac
    fi
    repository="${{repository#github.com/}}"
    if [ -n "$repository" ]; then
      GH_TOKEN="$({loom_bin} github-token --repository "$repository")" || exit $?
    else
      GH_TOKEN="$({loom_bin} github-token)" || exit $?
    fi
    export GH_TOKEN
    unset GITHUB_TOKEN
    ;;
  direct)
    if [ -z "$GH_TOKEN" ]; then
      echo "Loom-managed direct GitHub auth is missing GH_TOKEN" >&2
      exit 1
    fi
    unset GITHUB_TOKEN
    ;;
  disabled)
    echo "GitHub CLI access is disabled for this Loom session" >&2
    exit 1
    ;;
  "")
    echo "missing LOOM_GITHUB_AUTH_MODE; this environment was not prepared by Loom" >&2
    exit 1
    ;;
  *)
    echo "invalid LOOM_GITHUB_AUTH_MODE: $LOOM_GITHUB_AUTH_MODE" >&2
    exit 1
    ;;
esac
{launch}
"#
    )
}

/// Install the adapter only for sessions whose GitHub posture Loom stamped.
/// The run directory is session-private and persists across runtime recovery,
/// so rebuilding the small script on each launch is idempotent.
async fn install_github_cli_adapter(
    session_id: &str,
    extra_env: &[(String, String)],
) -> Result<Option<PathBuf>> {
    if !extra_env
        .iter()
        .any(|(name, _)| name == "LOOM_GITHUB_AUTH_MODE")
    {
        return Ok(None);
    }
    let github_cli = stock_github_cli();
    if github_cli.is_none() {
        tracing::warn!(
            session = session_id,
            "stock gh executable not found; installing fail-closed GitHub CLI adapter"
        );
    }
    let loom_bin = std::env::current_exe().context("resolving the loom executable")?;
    let directory = crate::db::run_dir(session_id).join("bin");
    tokio::fs::create_dir_all(&directory)
        .await
        .with_context(|| format!("creating {}", directory.display()))?;
    let adapter = directory.join("gh");
    tokio::fs::write(
        &adapter,
        github_cli_adapter_script(&loom_bin, github_cli.as_deref()),
    )
    .await
    .with_context(|| format!("writing {}", adapter.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700))
            .await
            .with_context(|| format!("setting permissions on {}", adapter.display()))?;
    }
    Ok(Some(directory))
}

fn prepend_path(env: &mut Vec<(String, String)>, directory: &Path) {
    let inherited = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let path = if inherited.is_empty() {
        directory.display().to_string()
    } else {
        format!("{}:{inherited}", directory.display())
    };
    if let Some((_, value)) = env.iter_mut().find(|(name, _)| name == "PATH") {
        *value = path;
    } else {
        env.push(("PATH".to_string(), path));
    }
}

/// Build the launch script the supervisor runs as `sh -c <script>`: prepend the
/// session's GitHub CLI adapter and the weaver bin dir to `$PATH`, run the
/// optional inner agent command, then `exec` the login shell. The session's
/// environment is delivered **out of band** (via the process environment — see
/// [`start_terminal`] / [`backend::new_session`]), not `export`-ed here, so
/// secret values never land on the child shell's argv. The `$PATH` prepend stays
/// in the script because it needs the shell to expand the inherited `$PATH` at
/// runtime; it carries no secret.
fn wrap_launch_script(
    inner: &str,
    weaver_dir: Option<&Path>,
    github_adapter_dir: Option<&Path>,
) -> String {
    let mut script = String::new();
    let path_prefix = github_adapter_dir
        .into_iter()
        .chain(weaver_dir)
        .map(sh_single_quote_path)
        .collect::<Vec<_>>()
        .join(":");
    if !path_prefix.is_empty() {
        script.push_str(&format!("export PATH={path_prefix}:\"$PATH\"; "));
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
    wrap_launch_script("", weaver_dir, None)
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
    /// Exact custom-agent definition to launch. `None` resolves a builtin, or
    /// looks up the named custom agent in the current registry (the adopt
    /// path).
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
    let mut env_owned = session_env(ctx.server_addr, ctx.branch_id, ctx.extra_env).await?;
    let github_adapter_dir = if let Some(session_id) = ctx
        .extra_env
        .iter()
        .find(|(name, _)| name == "LOOM_SESSION_ID")
        .map(|(_, value)| value.as_str())
    {
        let directory = install_github_cli_adapter(session_id, ctx.extra_env).await?;
        if let Some(directory) = &directory {
            prepend_path(&mut env_owned, directory);
        }
        directory
    } else {
        None
    };
    let env: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // Delivered via the process environment, not exported into the script —
    // see `wrap_launch_script` — so tokens/keys never appear on argv.
    let script = wrap_launch_script(inner, weaver_dir, github_adapter_dir.as_deref());
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
pub async fn session_env(
    server_addr: &str,
    branch_id: &str,
    extra_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let mut env: Vec<(String, String)> = vec![
        (
            "WEAVER_API".to_string(),
            crate::runner::api_url(server_addr).await?,
        ),
        ("WEAVER_BRANCH".to_string(), branch_id.to_string()),
    ];
    for (k, v) in extra_env {
        env.push((k.clone(), v.clone()));
    }
    Ok(env)
}

/// The `loom` binary path — a sibling of the running executable, falling back
/// to bare `loom` on `PATH`.
fn loom_bin_path() -> String {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(|d| d.join("loom").display().to_string())
        .unwrap_or_else(|| "loom".to_string())
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
/// declared value. A runtime with no ACP adapter (`supports_acp == false`, e.g.
/// a terminal-only custom agent) cannot be forced to `acp`.
/// Returns a key-free reason.
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
        return if meta.supports_acp {
            Ok("acp".to_string())
        } else {
            Err(format!(
                "agent '{}' does not support the acp protocol",
                meta.kind
            ))
        };
    }
    // Declared acp, requested terminal: only a builtin has a terminal launch
    // path; an acp custom agent has no terminal command to run.
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
/// carrying the same launch inputs but mapping them onto the adapter command,
/// `_meta`, and goal fields the protocol takes.
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
    pub fast: &'a str,
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

/// Build the [`AcpLaunch`] for a `protocol='acp'` session. For the builtin
/// claude this resolves the `claude-agent-acp` adapter, installs the
/// SessionStart-only hook bundle, and maps model/primer/mode into
/// `_meta.claudeCode.options`. For the builtin codex it resolves `codex-acp`
/// and maps the same inputs onto its env contract (`CODEX_CONFIG`,
/// `INITIAL_AGENT_MODE`, `DEFAULT_AUTH_REQUEST`); Codex's `agent` mode routes
/// approvals to Loom instead of its own model reviewer, and, being hookless,
/// takes the primer as its opening prompt like the terminal path. A custom
/// acp agent runs its `launch` command verbatim (setup stage first) as the
/// adapter, with no `_meta`.
pub async fn build_acp_launch(
    db: &Db,
    spec: &AcpLaunchSpec<'_>,
    open: AcpOpen,
) -> Result<AcpLaunch> {
    let is_fresh = matches!(open, AcpOpen::Fresh);
    let builtin_runtime = spec.custom.is_none().then_some(spec.runtime);
    let is_codex = builtin_runtime == Some("codex");
    let is_claude = builtin_runtime == Some("claude");
    let is_opencode = builtin_runtime == Some("opencode");
    let is_cursor_agent = builtin_runtime == Some("cursor-agent");
    let adapter_cmd = match spec.custom {
        // The `launch` command *is* the adapter; setup runs first, as for a
        // terminal custom agent.
        Some(agent) => join_shell(&[agent.setup.trim(), agent.launch.trim()]),
        None if is_codex => codex_acp_cmd(db).await,
        None if is_opencode => opencode_acp_cmd(db).await,
        None if is_cursor_agent => cursor_agent_acp_cmd(db).await,
        // The `claude` builtin, and any unrecognised builtin, use the claude
        // adapter — the historical default.
        None => claude_acp_cmd(db).await,
    };

    if is_claude && spec.prelude == "weaver" && !spec.restricted {
        // SessionStart only (see doc comment above); protocol turn edges and
        // the bypass posture make the other hooks redundant under ACP.
        let loom_bin = loom_bin_path();
        if let Err(e) = install_hooks(spec.work_dir, &loom_bin, HookMode::Acp).await {
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
    if (is_codex || is_opencode || is_cursor_agent) && goal_text.is_none() {
        // No appendSystemPrompt analogue on these adapters: a primer-only
        // launch seeds the primer positionally, mirroring the terminal path's
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

    let mut env = session_env(spec.server_addr, spec.branch_id, spec.extra_env).await?;
    push_env_default(&mut env, "LOOM_SESSION_ID", spec.session_id);
    if let Some(directory) = install_github_cli_adapter(spec.session_id, spec.extra_env).await? {
        prepend_path(&mut env, &directory);
    }
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
    let mut additional_directories = Vec::new();
    if is_codex {
        // Adapter-contract env, deferring to any operator-provided value.
        push_env_default(
            &mut env,
            "DEFAULT_AUTH_REQUEST",
            r#"{"methodId":"api-key"}"#,
        );
        let codex_mode = codex_acp_mode(spec.mode);
        additional_directories =
            configure_codex_acp(&mut env, spec.model, spec.effort, &codex_mode)?;
        push_env_default(&mut env, "INITIAL_AGENT_MODE", &codex_mode);
    }
    let mcp_servers =
        crate::mcp::acp_server_configs(&allowed_tools, Some(&mcp_snapshot), &env).await?;

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
        additional_directories,
        new_or_load,
        // Codex boots directly in its mapped mode via `INITIAL_AGENT_MODE`; a
        // post-setup `session/set_mode` would re-send a claude-flavored id it
        // does not advertise. Opencode and Cursor advertise their own mode
        // vocabularies, and a `session/set_mode` with an unknown id fails the
        // launch, so leave their mode to the adapter's default too.
        mode: (!is_codex && !is_opencode && !is_cursor_agent).then(|| spec.mode.to_string()),
        // Loading must preserve adapter-restored live choices: the user may
        // have changed either selector after launch. A blank selector (the
        // usual case for opencode/cursor, whose models are chosen live over
        // ACP) leaves the adapter on its own default.
        initial_model: (is_fresh && !spec.model.trim().is_empty())
            .then(|| spec.model.trim().to_string()),
        initial_effort: (is_fresh && !spec.effort.trim().is_empty())
            .then(|| spec.effort.trim().to_string()),
        initial_fast: (is_fresh && !spec.fast.trim().is_empty())
            .then(|| spec.fast.trim().to_string()),
        harness: spec
            .custom
            .map(|_| String::new())
            .unwrap_or_else(|| spec.runtime.to_string()),
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

/// An operator override for an ACP adapter command: an env var wins, then a
/// config setting.
async fn acp_cmd_override(db: &Db, env_var: &str, setting: &str) -> Option<String> {
    if let Ok(cmd) = std::env::var(env_var) {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            return Some(cmd.to_string());
        }
    }
    weaver_core::config::get(db, setting)
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Opencode's built-in ACP server (`opencode acp`); `WEAVER_OPENCODE_ACP_CMD`
/// or the `acp.opencode_cmd` setting override it.
async fn opencode_acp_cmd(db: &Db) -> String {
    acp_cmd_override(db, "WEAVER_OPENCODE_ACP_CMD", "acp.opencode_cmd")
        .await
        .unwrap_or_else(|| "opencode acp".to_string())
}

/// Cursor's built-in ACP server (`cursor-agent acp`, or `cursor agent acp`);
/// `WEAVER_CURSOR_ACP_CMD` or the `acp.cursor_cmd` setting override it.
async fn cursor_agent_acp_cmd(db: &Db) -> String {
    if let Some(cmd) = acp_cmd_override(db, "WEAVER_CURSOR_ACP_CMD", "acp.cursor_cmd").await {
        return cmd;
    }
    format!("{} acp", cursor_agent_bin().await.unwrap_or("cursor-agent"))
}

/// Apply Loom's Codex policy on top of the shared provider login and any
/// operator-supplied adapter config.
///
/// `agent` mode's on-request approval policy needs Loom in the loop to apply
/// its deterministic one-shot decisions, so the default reviewer is `user`
/// unless `CODEX_CONFIG` already names one. Weaver commands still need to
/// reach Loom from inside that sandbox, so `agent` mode also enables Codex's
/// network proxy for the control plane and, when GitHub auth is enabled, GitHub.
fn configure_codex_acp(
    env: &mut Vec<(String, String)>,
    model: &str,
    effort: &str,
    codex_mode: &str,
) -> Result<Vec<String>> {
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

        let sandbox = config
            .entry("sandbox_workspace_write".to_string())
            .or_insert_with(|| json!({}));
        let sandbox = sandbox
            .as_object_mut()
            .ok_or_else(|| anyhow!("CODEX_CONFIG.sandbox_workspace_write must be a JSON object"))?;
        sandbox.insert("network_access".to_string(), json!(true));
    }
    let features = config
        .entry("features".to_string())
        .or_insert_with(|| json!({}));
    let features = features
        .as_object_mut()
        .ok_or_else(|| anyhow!("CODEX_CONFIG.features must be a JSON object"))?;
    features.insert("apps".to_string(), json!(false));
    if codex_mode.trim() == CODEX_AGENT_MODE {
        let proxy = features
            .entry("network_proxy".to_string())
            .or_insert_with(|| json!({}));
        if proxy.is_boolean() {
            *proxy = json!({});
        }
        let proxy = proxy
            .as_object_mut()
            .ok_or_else(|| anyhow!("CODEX_CONFIG.features.network_proxy must be a JSON object"))?;
        proxy.insert("enabled".to_string(), json!(true));
        // The Docker control-plane alias resolves to a private address. Domain
        // allow rules alone do not bypass Codex's separate private-network gate.
        proxy.insert("allow_local_binding".to_string(), json!(true));
        let api_host = env
            .iter()
            .rev()
            .find_map(|(name, value)| (name == "WEAVER_API").then_some(value))
            .map(|value| reqwest::Url::parse(value))
            .transpose()
            .context("invalid session WEAVER_API URL")?
            .and_then(|url| url.host_str().map(str::to_string));
        let domains = proxy
            .entry("domains".to_string())
            .or_insert_with(|| json!({}));
        let domains = domains.as_object_mut().ok_or_else(|| {
            anyhow!("CODEX_CONFIG.features.network_proxy.domains must be a JSON object")
        })?;
        for host in ["127.0.0.1", "localhost"]
            .into_iter()
            .chain(api_host.as_deref())
        {
            domains.insert(host.to_string(), json!("allow"));
        }
        if env.iter().any(|(name, value)| {
            name == "LOOM_GITHUB_AUTH_MODE" && matches!(value.as_str(), "broker" | "direct")
        }) {
            // Credential brokerage and GitHub transport need separate network
            // access. Preserve an operator's explicit GitHub deny rule.
            for host in ["github.com", "api.github.com"] {
                domains.entry(host.to_string()).or_insert(json!("allow"));
            }
        }
    }

    // The adapter supplies its own sandbox policy on every turn. Configured
    // writable roots must also travel in the ACP session's additionalDirectories.
    let mut additional_directories: Vec<String> = config
        .get("sandbox_workspace_write")
        .and_then(|sandbox| sandbox.get("writable_roots"))
        .map(|roots| serde_json::from_value(roots.clone()))
        .transpose()
        .context("CODEX_CONFIG.sandbox_workspace_write.writable_roots must be an array of paths")?
        .unwrap_or_default();
    anyhow::ensure!(
        additional_directories
            .iter()
            .all(|root| Path::new(root).is_absolute()),
        "CODEX_CONFIG.sandbox_workspace_write.writable_roots must contain absolute paths"
    );
    let mut seen = HashSet::new();
    additional_directories.retain(|root| seen.insert(root.clone()));

    env.retain(|(name, _)| name != "CODEX_CONFIG");
    env.push((
        "CODEX_CONFIG".to_string(),
        Value::Object(config.clone()).to_string(),
    ));
    Ok(additional_directories)
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

/// The `_meta.claudeCode.options` object for the claude adapter — only the
/// fields that are configured (model, the primer as `appendSystemPrompt`, the
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
pub async fn install_hooks(work_dir: &Path, loom_bin: &str, mode: HookMode) -> Result<()> {
    let dir = work_dir.join(".claude");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("settings.local.json");
    let mut root: Value = match tokio::fs::read_to_string(&path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    let hooks = hooks_json(loom_bin, mode);
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

/// Resolves agents and runs non-interactive prompts against them. Interactive
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
                fast: "",
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
    launch.additional_directories.clear();
    launch.goal = None;
    launch.mode = Some("plan".to_string());
    launch.initial_model = None;
    launch.initial_effort = None;
    launch.initial_fast = None;
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

    #[cfg(unix)]
    fn write_executable(path: &Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, source).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

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
        wrap_launch_script(&inner, None, None)
    }

    #[test]
    fn bare_shell_script_just_execs_a_shell() {
        assert_eq!(bare_shell_script(None), "exec \"${SHELL:-/bin/sh}\"");
        // The `"shell"` runtime in the test helper builds the same bare shell.
        let script = launch_script("shell", None, None, LaunchMode::Fresh, "", "");
        assert_eq!(script, "exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    #[cfg(unix)]
    fn github_cli_adapter_refreshes_a_repository_token_for_every_call() {
        let directory = tempfile::tempdir().unwrap();
        let loom = directory.path().join("loom stub");
        let github_cli = directory.path().join("gh stock");
        let adapter = directory.path().join("gh");
        let calls = directory.path().join("token-calls");
        write_executable(
            &loom,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nprintf 'fresh-token\\n'\n",
                sh_single_quote_path(&calls)
            ),
        );
        write_executable(
            &github_cli,
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$GH_TOKEN\" \"${GITHUB_TOKEN-unset}\" \"$*\"\n",
        );
        write_executable(
            &adapter,
            &github_cli_adapter_script(&loom, Some(&github_cli)),
        );

        let invoke = |repository_flag: &[&str]| {
            std::process::Command::new(&adapter)
                .args(["pr", "view", "123"])
                .args(repository_flag)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LOOM_GITHUB_AUTH_MODE", "broker")
                .env("LOOM_SESSION_ID", "session")
                .env("LOOM_TOKEN", "session-token")
                .env("GITHUB_TOKEN", "stale-token")
                .output()
                .unwrap()
        };
        let first = invoke(&["--repo", "marin-community/marin"]);
        let second = invoke(&["-Rgithub.com/Open-Athena/mumwelt"]);

        assert!(first.status.success());
        assert_eq!(
            String::from_utf8(first.stdout).unwrap(),
            "fresh-token|unset|pr view 123 --repo marin-community/marin\n"
        );
        assert!(second.status.success());
        assert_eq!(
            String::from_utf8(second.stdout).unwrap(),
            "fresh-token|unset|pr view 123 -Rgithub.com/Open-Athena/mumwelt\n"
        );
        assert_eq!(
            std::fs::read_to_string(calls).unwrap(),
            "github-token --repository marin-community/marin\n\
             github-token --repository Open-Athena/mumwelt\n"
        );
    }

    #[test]
    fn github_cli_adapter_path_precedes_an_explicit_session_path() {
        let mut env = vec![("PATH".to_string(), "/custom/bin:/usr/bin".to_string())];

        prepend_path(&mut env, Path::new("/session/bin"));

        assert_eq!(
            env,
            vec![(
                "PATH".to_string(),
                "/session/bin:/custom/bin:/usr/bin".to_string()
            )]
        );
    }

    #[test]
    fn terminal_launch_keeps_the_github_adapter_ahead_of_the_loom_bin() {
        assert_eq!(
            wrap_launch_script(
                "",
                Some(Path::new("/loom/bin")),
                Some(Path::new("/session/bin"))
            ),
            "export PATH='/session/bin':'/loom/bin':\"$PATH\"; exec \"${SHELL:-/bin/sh}\""
        );
    }

    #[test]
    #[cfg(unix)]
    fn github_cli_adapter_fails_closed_without_a_stock_cli() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = directory.path().join("gh");
        write_executable(
            &adapter,
            &github_cli_adapter_script(Path::new("/missing/loom"), None),
        );

        let output = std::process::Command::new(&adapter)
            .env_clear()
            .env("LOOM_GITHUB_AUTH_MODE", "direct")
            .env("GH_TOKEN", "personal-token")
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(127));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "stock GitHub CLI executable not found\n"
        );
    }

    #[test]
    fn claude_accepts_versioned_model_names() {
        let metadata = CLAUDE_AGENT_TYPE.metadata();

        assert!(validate_model(&metadata, "claude-opus-4-8").is_ok());
    }

    #[test]
    fn validate_effort_does_not_gate_builtins_on_the_store() {
        let opencode = OPENCODE_AGENT_TYPE.metadata();
        assert!(validate_effort(&opencode, "glm-flash", "not-in-any-list").is_ok());
        assert!(validate_effort(&opencode, "glm-flash", "").is_ok());

        let mut custom = meta_for("helper", "terminal", false);
        custom.builtin = false;
        custom.efforts = vec![AgentChoice::leaf("low", "Low")];
        assert!(validate_effort(&custom, "", "low").is_ok());
        assert!(validate_effort(&custom, "", "high").is_err());
    }

    #[test]
    fn new_builtin_harnesses_are_registered() {
        for kind in ["opencode", "cursor-agent"] {
            assert!(
                builtin_agent_type(kind).is_some(),
                "{kind} should resolve to a builtin"
            );
            assert!(
                builtin_metadata().iter().any(|m| m.kind == kind),
                "{kind} should appear in the picker list"
            );
        }
        assert!(OPENCODE_AGENT_TYPE.metadata().supports_acp);
        assert!(CURSOR_AGENT_TYPE.metadata().supports_acp);
        assert!(OPENCODE_AGENT_TYPE.metadata().models.is_empty());
        assert!(CURSOR_AGENT_TYPE.metadata().models.is_empty());
    }

    #[test]
    fn new_harness_launch_commands() {
        let primer = Path::new("/x/primer.txt");
        let goal = Path::new("/x/goal.txt");

        // Cursor: `--model` verbatim (the exact advertised bracket string); a
        // fresh run takes the goal positionally, falling back to the primer
        // when there is no goal.
        let ctx = test_ctx(Some(goal), Some(primer), "grok-4.6[effort=high]", "");
        assert_eq!(
            cursor_agent_command("cursor-agent", &ctx, LaunchMode::Fresh),
            "cursor-agent --model grok-4.6[effort=high] \"$(cat '/x/goal.txt')\""
        );
        assert_eq!(
            cursor_agent_command("cursor agent", &ctx, LaunchMode::Adopt),
            "cursor agent --continue --model grok-4.6[effort=high]"
        );

        let oc = test_ctx(None, None, "opencode/gpt-5.4", "");
        assert_eq!(
            opencode_command(&oc, LaunchMode::Fresh),
            "opencode --model opencode/gpt-5.4"
        );
        assert_eq!(
            opencode_command(&oc, LaunchMode::Adopt),
            "opencode --continue --model opencode/gpt-5.4"
        );
    }

    #[test]
    fn a_builtin_without_an_acp_adapter_cannot_be_forced_to_acp() {
        let mut terminal_only = meta_for("terminal-only", "terminal", true);
        terminal_only.supports_acp = false;
        assert!(resolve_protocol(&terminal_only, Some("acp")).is_err());
        assert_eq!(
            resolve_protocol(&terminal_only, Some("terminal")).unwrap(),
            "terminal"
        );
        // The ACP builtins still opt in.
        assert_eq!(
            resolve_protocol(&meta_for("opencode", "acp", true), Some("terminal")).unwrap(),
            "terminal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_file_rejects_directories_and_unset_execute_bits() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!is_executable_file(&dir.path().join("missing")));

        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        assert!(!is_executable_file(&subdir));

        let plain = dir.path().join("plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(!is_executable_file(&plain));

        let script = dir.path().join("script");
        write_executable(&script, "#!/bin/sh\n");
        assert!(is_executable_file(&script));
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
        // command, so its session execs the login shell.
        let ctx = test_ctx(Some(Path::new("/x/goal.txt")), None, "", "");
        let a = custom_agent("bare", "", "", "");
        assert_eq!(custom_command(&a, &ctx, LaunchMode::Fresh), "");
        assert_eq!(
            wrap_launch_script(&custom_command(&a, &ctx, LaunchMode::Fresh), None, None,),
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
        // The session environment is delivered out of band (see
        // `start_terminal`), so no `export` may leak into the argv-visible script.
        assert!(
            !script.contains("export WEAVER_API") && !script.contains("export LOOM_TOKEN"),
            "env must not be baked into the launch script: {script}"
        );
    }

    #[test]
    fn claude_primer_rides_in_as_system_prompt_not_a_positional() {
        // Fresh and adopt both append the primer.
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
        // No positional `$(cat …)` prompt, which would take a turn on boot.
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
        let fresh = launch_script(
            "codex",
            Some(Path::new("/x/goal.txt")),
            None,
            LaunchMode::Fresh,
            "",
            "",
        );
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
        // Operator env vars are delivered via the process environment
        // (`CommandBuilder::env`, off argv), not shell-quoted into the script.
        // A value with shell metacharacters therefore needs no quoting and —
        // crucially — must not appear in the script at all, so `ps` can't read
        // it. The script is a pure function of the inner command, independent
        // of the env.
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
            mcp_servers: vec![json!({"name":"loom"})],
            additional_directories: vec!["/shared/cache".to_string()],
            new_or_load: NewOrLoad::New {
                cwd: PathBuf::from("/worktree"),
                meta: Some(json!({"provider":"options"})),
            },
            mode: Some("plan".to_string()),
            initial_model: Some("expensive".to_string()),
            initial_effort: Some("high".to_string()),
            initial_fast: None,
            harness: String::new(),
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
        assert!(summary.additional_directories.is_empty());
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
            additional_directories: Vec::new(),
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
            initial_fast: None,
            harness: String::new(),
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
        // A non-object root is reset rather than panicking.
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
            available: None,
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

    fn codex_environment_config(env: &[(String, String)]) -> Value {
        let config = env
            .iter()
            .find_map(|(name, value)| (name == "CODEX_CONFIG").then_some(value))
            .unwrap();
        serde_json::from_str(config).unwrap()
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
        let mut env = vec![
            (
                "CODEX_CONFIG".to_string(),
                r#"{"model":"operator","features":{"shell_snapshot":true,"apps":true}}"#
                    .to_string(),
            ),
            (
                "WEAVER_API".to_string(),
                "http://control-plane:4321".to_string(),
            ),
        ];

        configure_codex_acp(&mut env, "ignored", "high", "agent").unwrap();

        let config = codex_environment_config(&env);
        assert_eq!(config["model"], "operator");
        assert_eq!(config["approvals_reviewer"], "user");
        assert_eq!(config["sandbox_workspace_write"]["network_access"], true);
        assert_eq!(config["features"]["shell_snapshot"], true);
        assert_eq!(config["features"]["apps"], false);
        assert_eq!(config["features"]["network_proxy"]["enabled"], true);
        assert_eq!(
            config["features"]["network_proxy"]["allow_local_binding"],
            true
        );
        for host in ["127.0.0.1", "localhost", "control-plane"] {
            assert_eq!(
                config["features"]["network_proxy"]["domains"][host],
                "allow"
            );
        }
        assert!(config["features"]["network_proxy"]["domains"]
            .get("*")
            .is_none());
    }

    #[test]
    fn codex_acp_github_network_access_follows_the_credential_policy() {
        for mode in ["broker", "direct", "disabled", ""] {
            let mut env = vec![("LOOM_GITHUB_AUTH_MODE".to_string(), mode.to_string())];
            configure_codex_acp(&mut env, "", "", "agent").unwrap();
            let config = codex_environment_config(&env);
            let domains = &config["features"]["network_proxy"]["domains"];
            for host in ["github.com", "api.github.com"] {
                if matches!(mode, "broker" | "direct") {
                    assert_eq!(domains[host], "allow", "{mode}: {host}");
                } else {
                    assert!(domains.get(host).is_none(), "{mode}: {host}");
                }
            }
            assert!(domains.get("*").is_none());
        }

        let mut env = vec![
            ("LOOM_GITHUB_AUTH_MODE".to_string(), "broker".to_string()),
            (
                "CODEX_CONFIG".to_string(),
                json!({"features": {"network_proxy": {"domains": {
                    "api.github.com": "deny", "packages.example.com": "allow"
                }}}})
                .to_string(),
            ),
        ];
        configure_codex_acp(&mut env, "", "", "agent").unwrap();
        let config = codex_environment_config(&env);
        let domains = &config["features"]["network_proxy"]["domains"];
        assert_eq!(domains["api.github.com"], "deny");
        assert_eq!(domains["packages.example.com"], "allow");
    }

    #[test]
    fn codex_acp_rejects_invalid_writable_roots() {
        for roots in [
            serde_json::json!("/cache"),
            serde_json::json!([1]),
            serde_json::json!(["relative/cache"]),
            serde_json::json!([""]),
            Value::Null,
        ] {
            let mut env = vec![(
                "CODEX_CONFIG".to_string(),
                serde_json::json!({"sandbox_workspace_write": {"writable_roots": roots}})
                    .to_string(),
            )];
            let error = configure_codex_acp(&mut env, "", "", "agent").unwrap_err();
            assert!(error.to_string().contains("writable_roots"), "{error:#}");
        }
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
            assert!(
                config.get("sandbox_workspace_write").is_none(),
                "{mode} must not change sandbox networking"
            );
            assert!(
                config["features"].get("network_proxy").is_none(),
                "{mode} must not start the sandbox network proxy"
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
            r#"["Read(./**)","mcp__loom__github_issue_view","mcp__loom__github_issue_comment"]"#,
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
                "mcp__loom__github_issue_view",
                "mcp__loom__github_issue_comment"
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
                fast: "",
                goal_file: None,
                primer_file: None,
                extra_env: &extra_env,
                env_clear: true,
                mode: "default",
                prelude: "none",
                restricted: true,
                allowed_tools: r#"["Read(./**)","mcp__loom__github_issue_edit"]"#,
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
        assert_eq!(launch.mcp_servers[0]["name"], "loom");
        assert!(launch.mcp_servers[0]["command"]
            .as_str()
            .is_some_and(|command| std::path::Path::new(command).is_absolute()));
        assert_eq!(launch.mcp_servers[0]["args"], json!(["mcp", "serve"]));
        assert_eq!(
            launch.mcp_servers[0]["env"],
            json!([
                {
                    "name": "LOOM_MCP_ALLOWED_TOOLS",
                    "value": "[\"github_issue_edit\"]"
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
