//! Agent registry and launch management for terminal sessions, ACP sessions,
//! and fresh ACP one-shot judgement calls.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    pub(crate) fn list(pairs: &[(&str, &str)]) -> Vec<AgentChoice> {
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
    /// The launch modes this agent can honour. A builtin lists the Loom modes
    /// it has an adapter mode for; a custom agent states none, and accepts
    /// whatever its own command understands.
    pub modes: Vec<String>,
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

pub use crate::agent_kind::{
    acp_mode_for, auto_approves_permissions, BuiltinAgentKind, CODEX_AGENT_MODE, DEFAULT_ACP_MODE,
};
pub use crate::harnesses::claude::{
    claude_projects_dir, latest_claude_session_id, seed_claude_launch_gates,
};

/// The builtin agents' metadata, in picker order.
pub fn builtin_metadata() -> Vec<AgentMetadata> {
    crate::harnesses::all_builtins()
        .into_iter()
        .map(|harness| harness.metadata())
        .collect()
}

/// The builtin agent for `kind`, if it names one. Custom agents live in the
/// database and are resolved by [`resolve`]; this covers only the code-shipped
/// runtimes, which need no DB lookup.
pub fn builtin_agent_type(kind: &str) -> Option<&'static dyn AgentType> {
    crate::harnesses::builtin_for_id(kind).map(|harness| harness as &'static dyn AgentType)
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

/// Whether a code-shipped harness binary can be probed on this machine.
pub async fn builtin_available_kind(kind: BuiltinAgentKind) -> bool {
    crate::harnesses::builtin_for(kind).available().await
}

/// The ACP adapter command for a builtin harness, using the same override
/// precedence as a live launch.
pub async fn acp_command_for(db: &Db, kind: &str) -> Option<String> {
    let harness = crate::harnesses::builtin_for_id(kind)?;
    Some(harness.acp_command(db.clone()).await)
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
/// the picker currently offers.
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

/// [`validate_model`] against everything the picker offers, including the
/// models a catalogue probe has stored for a builtin harness.
///
/// `metadata_for` deliberately leaves the stored catalogue out of the resolver
/// snapshot, so a fixed-list harness has to consult the store here or an
/// ACP-discovered model could be selected in the picker and then rejected on
/// save and launch.
pub async fn validate_model_of(
    db: &Db,
    metadata: &AgentMetadata,
    model: &str,
) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() || metadata.accepts_raw_model || !metadata.builtin {
        return validate_model(metadata, model);
    }
    if validate_model(metadata, model).is_ok() {
        return Ok(());
    }
    let Ok(rows) = crate::catalog::stored_models(db, &metadata.kind).await else {
        return validate_model(metadata, model);
    };
    if rows.iter().any(|id| id == model) {
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

/// Validate a fast-mode selection: empty (the agent default) or one of the
/// boolean spellings the picker offers. The adapter's live option set is still
/// the authority at launch; this only catches a typo before a profile is saved.
pub fn validate_fast(fast: &str) -> Result<(), String> {
    match fast.trim() {
        "" | "true" | "false" => Ok(()),
        other => Err(format!("fast must be 'true' or 'false', got '{other}'")),
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
            modes: Vec::new(),
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
pub(crate) fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn sh_single_quote_path(path: &Path) -> String {
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

pub(crate) async fn start_terminal(
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
pub(crate) fn loom_bin_path() -> String {
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
    /// The launch permission posture (`bypassPermissions`, `acceptEdits`, …),
    /// or the adapter's own id when `mode_from_live` is set.
    pub mode: &'a str,
    /// `mode` is the adapter's own id, persisted by the live session, and is
    /// replayed verbatim on a respawn. The adapter is the authority on its
    /// vocabulary, so it is not translated back through Loom's postures.
    pub mode_from_live: bool,
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

/// Build the [`AcpLaunch`] for a `protocol='acp'` session. Each builtin
/// harness contributes its facts through [`crate::harnesses::BuiltinHarness`]
/// — the adapter command, the `_meta` it takes (claude's
/// `_meta.claudeCode.options`), its env contract (codex's `CODEX_CONFIG`,
/// `INITIAL_AGENT_MODE`, `DEFAULT_AUTH_REQUEST`), and whether the primer
/// seeds the opening prompt. A custom acp agent runs its `launch` command
/// verbatim (setup stage first) as the adapter, with no `_meta`.
pub async fn build_acp_launch(
    db: &Db,
    spec: &AcpLaunchSpec<'_>,
    open: AcpOpen,
) -> Result<AcpLaunch> {
    let is_fresh = matches!(open, AcpOpen::Fresh);
    let harness = if spec.custom.is_none() {
        crate::harnesses::builtin_for_id(spec.runtime)
    } else {
        None
    };
    let adapter_cmd = match spec.custom {
        // The `launch` command *is* the adapter; setup runs first, as for a
        // terminal custom agent.
        Some(agent) => join_shell(&[agent.setup.trim(), agent.launch.trim()]),
        // The `claude` builtin, and any unrecognised builtin, use the claude
        // adapter — the historical default.
        None => match harness {
            Some(harness) => harness.acp_command(db.clone()).await,
            None => {
                crate::harnesses::builtin_for(BuiltinAgentKind::Claude)
                    .acp_command(db.clone())
                    .await
            }
        },
    };

    if harness.is_some_and(|h| h.installs_prelude_hooks())
        && spec.prelude == "weaver"
        && !spec.restricted
    {
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
    if harness.is_some_and(|h| h.seeds_primer_as_goal()) && goal_text.is_none() {
        // No appendSystemPrompt analogue on these adapters: a primer-only
        // launch seeds the primer positionally, mirroring the terminal path's
        // `goal_file.or(primer_file)`.
        goal_text = primer_text.clone();
    }
    let contract = crate::harnesses::AcpContract {
        model: spec.model,
        effort: spec.effort,
        mode: spec.mode,
        mode_from_live: spec.mode_from_live,
        restricted: spec.restricted,
        allowed_tools_json: spec.allowed_tools,
        primer_text: primer_text.as_deref(),
    };
    let meta = harness.and_then(|h| h.acp_meta(&contract));

    let mut env = session_env(spec.server_addr, spec.branch_id, spec.extra_env).await?;
    crate::harnesses::push_env_default(&mut env, "LOOM_SESSION_ID", spec.session_id);
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
    if let Some(harness) = harness {
        // Adapter-contract env, deferring to any operator-provided value.
        additional_directories = harness.prepare_acp_env(&mut env, &contract)?;
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
        // A harness that boots in its mode via the launch environment (Codex's
        // `INITIAL_AGENT_MODE`) skips the post-setup `session/set_mode`, which
        // would re-send a claude-flavored id it does not advertise. Every
        // other harness takes the id it advertises.
        mode: if harness.is_some_and(|h| h.boots_in_acp_mode()) {
            None
        } else if spec.mode_from_live {
            Some(spec.mode.trim().to_string()).filter(|mode| !mode.is_empty())
        } else {
            launch_mode(spec.runtime, spec.custom, spec.mode)?
        },
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

/// The mode id to send through `session/set_mode` for a launch.
///
/// A custom agent's mode namespace is the operator's own, so it passes through
/// as written. A builtin is translated into the id its adapter advertises and
/// rejected when it has no equivalent, so `plan` cannot silently become a
/// full-access default.
fn launch_mode(runtime: &str, custom: Option<&CustomAgent>, mode: &str) -> Result<Option<String>> {
    if custom.is_some() {
        return Ok(Some(mode.to_string()));
    }
    let Some(kind) = BuiltinAgentKind::parse(runtime) else {
        return Ok(Some(mode.to_string()));
    };
    match acp_mode_for(kind, mode) {
        Some(mapped) => Ok(Some(mapped.to_string())),
        None => anyhow::bail!(
            "{} has no '{mode}' mode (modes: {})",
            kind.as_str(),
            crate::harnesses::acp_modes(kind).join(", ")
        ),
    }
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
    /// cloned for adapter/config parity, then given the existing history MCP
    /// tools with a read-only scoped credential. Adapter, model-selection,
    /// timeout, and output failures degrade to the handoff fallback.
    pub async fn summarize_handoff(
        &self,
        runtime: &str,
        prompt: &str,
        incoming: &AcpLaunch,
        history_token: &str,
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
        let allowed_tools = vec![
            crate::mcp::builtin_permission_rule("session", "history"),
            crate::mcp::builtin_permission_rule("session", "search"),
        ];
        let mut history_env = incoming.env.clone();
        history_env.retain(|(name, _)| name != "LOOM_TOKEN");
        history_env.push(("LOOM_TOKEN".to_string(), history_token.to_string()));
        let mcp_servers =
            match crate::mcp::acp_server_configs(&allowed_tools, None, &history_env).await {
                Ok(servers) => servers,
                Err(error) => {
                    tracing::warn!(%error, "handoff history tools unavailable");
                    return HandoffSummary {
                        text: None,
                        model: None,
                        status: "unavailable",
                    };
                }
            };
        let launch = transient_prompt_launch(incoming, mcp_servers, &allowed_tools);
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
                mode_from_live: false,
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
            Ok(launch) => transient_prompt_launch(&launch, Vec::new(), &[]),
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

/// Apply the common transient-session policy. MCP descriptors are standard ACP;
/// Claude additionally requires its adapter-specific tool allowlist in metadata.
fn transient_prompt_launch(
    incoming: &AcpLaunch,
    mcp_servers: Vec<Value>,
    allowed_tools: &[String],
) -> AcpLaunch {
    let mut launch = incoming.clone();
    let mut env = BTreeMap::new();
    if !launch.env_clear {
        env.extend(std::env::vars());
    }
    env.extend(launch.env.clone());
    env.retain(|name, _| {
        !name.starts_with("WEAVER_")
            && !name.starts_with("LOOM_")
            && !crate::env_policy::is_boot_only_env(name)
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
    launch.mcp_servers = mcp_servers;
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
            options.insert("allowedTools".to_string(), json!(allowed_tools));
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
    use crate::harnesses::claude::{claude_command, combine_args, effort_args, model_args};
    use crate::harnesses::codex::codex_command;
    use crate::harnesses::cursor::cursor_agent_command;
    use crate::harnesses::opencode::opencode_command;
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
        let metadata = crate::harnesses::claude::HARNESS.metadata();

        assert!(validate_model(&metadata, "claude-opus-4-8").is_ok());
    }

    #[tokio::test]
    async fn a_stored_catalogue_model_saves_though_the_static_list_omits_it() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let metadata = crate::harnesses::codex::HARNESS.metadata();
        // Codex has a fixed list, so a model the probe discovered is not in it.
        assert!(validate_model(&metadata, "gpt-7-nova").is_err());
        sqlx::query(
            "INSERT INTO harness_model_catalog (harness, model_id, label)
             VALUES ('codex', 'gpt-7-nova', 'GPT-7 Nova')",
        )
        .execute(&db)
        .await
        .unwrap();
        assert!(validate_model_of(&db, &metadata, "gpt-7-nova")
            .await
            .is_ok());
        // The static list still governs models the store does not list.
        assert!(validate_model_of(&db, &metadata, "gpt-99-imaginary")
            .await
            .is_err());
        // A raw-model harness ignores both lists entirely.
        assert!(validate_model_of(
            &db,
            &crate::harnesses::claude::HARNESS.metadata(),
            "anything"
        )
        .await
        .is_ok());
    }

    #[test]
    fn validate_effort_does_not_gate_builtins_on_the_store() {
        let opencode = crate::harnesses::opencode::HARNESS.metadata();
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
        assert!(crate::harnesses::opencode::HARNESS.metadata().supports_acp);
        assert!(crate::harnesses::cursor::HARNESS.metadata().supports_acp);
        assert!(crate::harnesses::opencode::HARNESS
            .metadata()
            .models
            .is_empty());
        assert!(crate::harnesses::cursor::HARNESS
            .metadata()
            .models
            .is_empty());
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
            "cursor-agent --model 'grok-4.6[effort=high]' \"$(cat '/x/goal.txt')\""
        );
        assert_eq!(
            cursor_agent_command("cursor agent", &ctx, LaunchMode::Adopt),
            "cursor agent --continue --model 'grok-4.6[effort=high]'"
        );

        let oc = test_ctx(None, None, "opencode/gpt-5.4", "");
        assert_eq!(
            opencode_command(&oc, LaunchMode::Fresh),
            "opencode --model 'opencode/gpt-5.4'"
        );
        assert_eq!(
            opencode_command(&oc, LaunchMode::Adopt),
            "opencode --continue --model 'opencode/gpt-5.4'"
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

        let summary = transient_prompt_launch(&incoming, Vec::new(), &[]);
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

        let transient = transient_prompt_launch(&incoming, Vec::new(), &[]);
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
        let tools = vec![crate::mcp::builtin_permission_rule("session", "history")];
        let servers = vec![json!({"name":"loom", "command":"loom", "args":["mcp", "serve"]})];
        let transient = transient_prompt_launch(&incoming, servers.clone(), &tools);
        assert_eq!(transient.mcp_servers, servers);
        let NewOrLoad::New {
            meta: Some(meta), ..
        } = transient.new_or_load
        else {
            panic!("expected Claude metadata");
        };
        assert_eq!(meta["claudeCode"]["options"]["allowedTools"], json!(tools));
        assert_eq!(meta["claudeCode"]["options"]["tools"], json!([]));
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
            modes: Vec::new(),
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
    fn opencode_and_cursor_map_onto_the_modes_their_adapters_advertise() {
        // Both advertise exactly two modes; neither has accepted-edits or bypass.
        for kind in [BuiltinAgentKind::OpenCode, BuiltinAgentKind::CursorAgent] {
            assert_eq!(acp_mode_for(kind, "plan"), Some("plan"));
            assert_eq!(acp_mode_for(kind, "default"), acp_mode_for(kind, "auto"));
            assert_eq!(acp_mode_for(kind, "acceptEdits"), None);
            assert_eq!(acp_mode_for(kind, "bypassPermissions"), None);
            assert_eq!(
                crate::harnesses::acp_modes(kind),
                vec!["auto", "default", "plan"],
                "{} must offer only what it can honour",
                kind.as_str()
            );
        }
        // The default posture is the one each adapter uses natively.
        assert_eq!(
            acp_mode_for(BuiltinAgentKind::OpenCode, "default"),
            Some("build")
        );
        assert_eq!(
            acp_mode_for(BuiltinAgentKind::CursorAgent, "default"),
            Some("agent")
        );
    }

    #[test]
    fn every_harness_offers_the_postures_it_can_map() {
        for kind in BuiltinAgentKind::ALL {
            for mode in crate::harnesses::acp_modes(kind) {
                assert!(
                    acp_mode_for(kind, mode).is_some(),
                    "{} advertises {mode} without a mapping",
                    kind.as_str()
                );
            }
        }
        assert_eq!(
            crate::harnesses::acp_modes(BuiltinAgentKind::Claude).len(),
            5
        );
    }

    /// A respawn feeds `Session.current_mode` — the adapter's id, not a Loom
    /// posture — back through `launch_mode`. Rejecting those ids made every
    /// opencode (`build`) and cursor (`agent`/`ask`) relay respawn fail.
    #[test]
    fn launch_mode_replays_a_persisted_live_mode_id() {
        for (runtime, live) in [
            ("opencode", "build"),
            ("opencode", "plan"),
            ("cursor-agent", "agent"),
            ("cursor-agent", "ask"),
            ("codex", "read-only"),
            ("claude", "bypassPermissions"),
        ] {
            assert_eq!(
                launch_mode(runtime, None, live).unwrap(),
                Some(live.to_string()),
                "{runtime} must replay its persisted '{live}'"
            );
        }
        // A posture the harness cannot express is still rejected rather than
        // silently downgraded to a different permission posture.
        assert!(launch_mode("opencode", None, "bypassPermissions").is_err());
        assert!(launch_mode("cursor-agent", None, "acceptEdits").is_err());
    }

    /// The reported failure: a respawn replays the adapter's persisted mode id
    /// (`build`) through `build_acp_launch`, which used to reject it before the
    /// adapter was even started.
    #[tokio::test]
    async fn build_acp_launch_replays_opencode_s_persisted_mode() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launch = build_acp_launch(
            &db,
            &AcpLaunchSpec {
                session_id: "session-1",
                branch_id: "branch-1",
                runtime: "opencode",
                work_dir: work_dir.path(),
                server_addr: "127.0.0.1:7878",
                model: "",
                effort: "",
                fast: "",
                goal_file: None,
                primer_file: None,
                extra_env: &[],
                env_clear: false,
                mode: "build",
                mode_from_live: false,
                prelude: "none",
                restricted: false,
                allowed_tools: "[]",
                mcp_access:
                    r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[],"custom_servers":[]}"#,
                custom: None,
            },
            AcpOpen::Fresh,
        )
        .await
        .expect("a live session's own mode id must survive a respawn");
        assert_eq!(launch.mode.as_deref(), Some("build"));
    }

    /// The Opencode builtin runs every ACP session against its own data home
    /// (credentials especially); a restricted session is scoped but never
    /// seeded with the deploy's store.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_opencode_launch_runs_against_its_own_data_home() {
        struct EnvGuard(Option<std::ffi::OsString>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("WEAVER_HOME", value),
                    None => std::env::remove_var("WEAVER_HOME"),
                }
            }
        }
        let home = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(shared.path().join("opencode")).unwrap();
        std::fs::write(shared.path().join("opencode/opencode.db"), b"deploy").unwrap();
        let _home = EnvGuard(std::env::var_os("WEAVER_HOME"));
        std::env::set_var("WEAVER_HOME", home.path());
        let _shared = EnvGuard(std::env::var_os("XDG_DATA_HOME"));
        std::env::set_var("XDG_DATA_HOME", shared.path());

        let db = crate::db::connect_in_memory().await.unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launch = build_acp_launch(
            &db,
            &AcpLaunchSpec {
                session_id: "acp-opencode-7",
                branch_id: "branch-1",
                runtime: "opencode",
                work_dir: work_dir.path(),
                server_addr: "127.0.0.1:7878",
                model: "",
                effort: "",
                fast: "",
                goal_file: None,
                primer_file: None,
                extra_env: &[],
                env_clear: false,
                mode: "build",
                mode_from_live: false,
                prelude: "none",
                restricted: false,
                allowed_tools: "[]",
                mcp_access:
                    r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[],"custom_servers":[]}"#,
                custom: None,
            },
            AcpOpen::Fresh,
        )
        .await
        .expect("the opencode launch builds");
        let scoped = |session: &str| home.path().join("opencode-data").join(session);
        assert_eq!(
            launch
                .env
                .iter()
                .find(|(name, _)| name == "XDG_DATA_HOME")
                .map(|(_, value)| value.as_str()),
            Some(scoped("acp-opencode-7").display().to_string().as_str()),
            "the adapter runs against its own data home"
        );
        assert!(
            scoped("acp-opencode-7")
                .join("opencode/opencode.db")
                .is_file(),
            "the deploy store seeds the session's home"
        );

        let restricted = build_acp_launch(
            &db,
            &AcpLaunchSpec {
                session_id: "acp-opencode-restricted",
                branch_id: "branch-1",
                runtime: "opencode",
                work_dir: work_dir.path(),
                server_addr: "127.0.0.1:7878",
                model: "",
                effort: "",
                fast: "",
                goal_file: None,
                primer_file: None,
                extra_env: &[],
                env_clear: false,
                mode: "build",
                mode_from_live: false,
                prelude: "none",
                restricted: true,
                allowed_tools: "[]",
                mcp_access:
                    r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[],"custom_servers":[]}"#,
                custom: None,
            },
            AcpOpen::Fresh,
        )
        .await
        .expect("the restricted opencode launch builds");
        assert_eq!(
            restricted
                .env
                .iter()
                .find(|(name, _)| name == "XDG_DATA_HOME")
                .map(|(_, value)| value.as_str()),
            Some(
                scoped("acp-opencode-restricted")
                    .display()
                    .to_string()
                    .as_str()
            ),
            "a restricted session is still scoped"
        );
        assert!(
            !scoped("acp-opencode-restricted")
                .join("opencode/opencode.db")
                .exists(),
            "the deploy's credentials must not reach a restricted adapter"
        );
    }

    /// An adapter can advertise a mode Loom has never heard of — an Opencode
    /// primary agent from `OPENCODE_CONFIG_CONTENT`, say. A respawn replays the
    /// persisted id verbatim rather than failing on Loom's own allowlist.
    #[tokio::test]
    async fn build_acp_launch_replays_an_advertised_mode_loom_does_not_know() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launch = build_acp_launch(
            &db,
            &AcpLaunchSpec {
                session_id: "session-1",
                branch_id: "branch-1",
                runtime: "opencode",
                work_dir: work_dir.path(),
                server_addr: "127.0.0.1:7878",
                model: "",
                effort: "",
                fast: "",
                goal_file: None,
                primer_file: None,
                extra_env: &[],
                env_clear: false,
                mode: "review-agent",
                mode_from_live: true,
                prelude: "none",
                restricted: false,
                allowed_tools: "[]",
                mcp_access:
                    r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[],"custom_servers":[]}"#,
                custom: None,
            },
            AcpOpen::Fresh,
        )
        .await
        .expect("an adapter-advertised mode must replay verbatim");
        assert_eq!(launch.mode.as_deref(), Some("review-agent"));
        // Without the live flag the same id is a posture Loom cannot express.
        assert!(launch_mode("opencode", None, "review-agent").is_err());
    }

    #[test]
    fn fast_accepts_only_the_boolean_spellings() {
        for ok in ["", "true", "false", " true "] {
            assert!(validate_fast(ok).is_ok(), "rejected '{ok}'");
        }
        for bad in ["yes", "on", "TRUE", "1"] {
            assert!(validate_fast(bad).is_err(), "accepted '{bad}'");
        }
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
        assert!(auto_approves_permissions(
            acp_mode_for(BuiltinAgentKind::Codex, "bypassPermissions").unwrap()
        ));
        // Everything else must still prompt.
        for mode in ["auto", "acceptEdits", "default", "plan", "read-only", ""] {
            assert!(
                !auto_approves_permissions(mode),
                "{mode} must not auto-approve"
            );
        }
    }

    #[test]
    fn cursor_agent_mode_is_not_codex_agent_mode() {
        // Codex's `agent` routes one-shot approvals to Loom; Cursor's `agent`
        // prompts the client for every tool call. The same id must not turn
        // Cursor's ordinary mode into a no-prompt posture.
        use crate::agent_kind::{auto_approves_permissions_for, BuiltinAgentKind};
        assert!(auto_approves_permissions_for(
            Some(BuiltinAgentKind::Codex),
            "agent"
        ));
        assert!(!auto_approves_permissions_for(
            Some(BuiltinAgentKind::CursorAgent),
            "agent"
        ));
        assert!(!auto_approves_permissions_for(
            Some(BuiltinAgentKind::OpenCode),
            "build"
        ));
        // An unknown harness keeps the historical reading.
        assert!(auto_approves_permissions_for(None, "agent"));
        assert!(auto_approves_permissions_for(
            Some(BuiltinAgentKind::Claude),
            "bypassPermissions"
        ));
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
                mode_from_live: false,
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
