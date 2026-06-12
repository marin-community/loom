//! loom — the orchestration CLI.
//!
//! Most subcommands talk to the running loom daemon over HTTP (session
//! lifecycle, archive, adopt). `serve` runs the daemon itself;
//! `start`/`stop`/`restart`/`status` manage its background lifecycle. To
//! interact with an agent, `attach` to its tmux (the browser terminal is the
//! other interaction surface).

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use loom::client::{self, Client};

#[derive(Parser)]
#[command(
    name = "loom",
    version,
    about = "Orchestrate concurrent agent workstreams"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the loom server (REST API + Vue UI + monitor loop).
    Serve {
        #[arg(long)]
        addr: Option<String>,
    },
    /// Show the running daemon's status.
    Status,
    /// Start the loom daemon in the background.
    Start,
    /// Stop the loom daemon.
    Stop,
    /// Stop and re-start the loom daemon.
    Restart,

    /// Manage sessions: launch, poll, wait, send, break, preview.
    ///
    /// A uniform surface for driving a child session — start one, watch it, and
    /// interact with its agent pane:
    ///
    ///     loom session launch "Add a /health endpoint and a test for it"
    ///     loom session poll weaver/health      # one-shot status
    ///     loom session wait weaver/health      # block until done / needs you
    ///     loom session send weaver/health "try the curl again"
    ///     loom session break weaver/health     # interrupt the current turn
    ///     loom session preview weaver/health   # peek at the tmux screen
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Manage overlookers: periodic / triggered watch programs over the fleet.
    ///
    /// An overlooker wakes on a trigger (a cron tick or a session event),
    /// surveys the fleet, and acts — marking a session, nudging a stuck one,
    /// escalating to you. Author one as a plain file an agent can edit, then
    /// register it and iterate with `--dry-run`:
    ///
    ///     loom overlooker programs                 # the builtin programs that ship with loom
    ///     loom overlooker new test-watch          # scaffold ~/.weaver/overlookers/test-watch.py
    ///     loom overlooker add status --cron "0 * * * *" --capabilities observe,mark,escalate
    ///     loom overlooker run status --dry-run     # simulate; mutating actions are stubbed
    ///     loom overlooker enable status            # arm it
    ///     loom overlooker ls                       # the fleet of watchers
    Overlooker {
        #[command(subcommand)]
        cmd: OverlookerCmd,
    },
    /// Deprecated alias for `loom session launch`.
    #[command(hide = true)]
    Launch(LaunchOpts),
    /// List active sessions.
    Ps,
    /// Show the repo's issue board (every issue across branches + backlog).
    Issues {
        /// Include closed issues.
        #[arg(long)]
        all: bool,
        /// Show only the unclaimed backlog.
        #[arg(long)]
        backlog: bool,
    },
    /// Show one session's details.
    Show { branch: String },
    /// Attach your terminal to a session's tmux.
    Attach { branch: String },
    /// Archive a session: tear down tmux + worktree, keep branch + history.
    Archive { branch: String },
    /// Recreate the tmux session for an orphaned session.
    Adopt { branch: String },
    /// Remove a session (worktree + tmux + DB row).
    Rm {
        branch: String,
        #[arg(long)]
        keep_branch: bool,
    },
    /// Open the loom web UI in a browser.
    Open,
    /// Generate shell completions.
    Completions { shell: clap_complete::Shell },
}

/// Subcommands under `loom session` — the uniform way to drive a child session.
#[derive(Subcommand)]
enum SessionCmd {
    /// Launch a new session: worktree + tmux + agent, seeded with a task.
    ///
    /// The positional argument is the task the agent should work on — it
    /// becomes the branch goal and the agent's opening prompt:
    ///
    ///     loom session launch "Add a /health endpoint and a test for it"
    ///
    /// The branch name (`weaver/<slug>`) is derived from the task; override it
    /// with `--name`. To pick up existing work instead of describing a new
    /// task, use `--claim <id>`, `--issue <n>`, or `--branch <name>`.
    Launch(LaunchOpts),
    /// Poll a session's status: lifecycle + the agent's attention and message.
    Poll {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
    },
    /// Block until a session finishes or its agent needs you.
    ///
    /// Polls until the session reaches a terminal lifecycle state (`done` /
    /// `error` / `archived`) or is lost (`orphaned`), or — unless
    /// `--lifecycle-only` — until its agent raises attention to
    /// `attention`/`blocked`. Prints why it woke. Exits non-zero if `--timeout`
    /// elapses first.
    Wait {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Give up after this many seconds (0 = wait indefinitely).
        #[arg(long, default_value = "1800")]
        timeout: u64,
        /// Seconds between polls.
        #[arg(long, default_value = "3")]
        interval: u64,
        /// Wake only on a lifecycle change; ignore the agent's attention.
        #[arg(long)]
        lifecycle_only: bool,
    },
    /// Type a message into a session's agent pane (and submit it to trigger an
    /// agent round).
    Send {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// The message to type. Multiple words are joined, so quoting is
        /// optional.
        message: Vec<String>,
        /// Type the message but don't press Enter — stage it without submitting.
        #[arg(long)]
        no_enter: bool,
    },
    /// Send a break (Escape) to a session — interrupt the agent's current turn.
    Break {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
    },
    /// Print a session's recent tmux screen.
    #[command(alias = "sp")]
    Preview {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Extra scrollback lines above the visible screen (0 = visible only).
        #[arg(long, default_value = "0")]
        lines: usize,
    },
}

/// Subcommands under `loom overlooker` — the operator + authoring surface. A
/// thin client over the REST API ("the API is the CLI").
#[derive(Subcommand)]
enum OverlookerCmd {
    /// Scaffold a starter program file at `~/.weaver/overlookers/<name>.py`.
    ///
    /// Writes a commented Python template against the program contract (the
    /// fleet over `$WEAVER_API`, round config in `$WEAVER_OVERLOOKER`, result
    /// JSON on stdout), then prints the path. Edit it, then register it with
    /// `loom overlooker add <name> --program <path>`.
    New {
        /// The overlooker name; also the file stem (`<name>.py`).
        name: String,
    },
    /// List the builtin programs that ship with loom (GET /api/overlookers/programs).
    Programs {
        /// Print one program's script source instead of the table, e.g.
        /// `--source builtin:archive-merged` — a working example to start from.
        #[arg(long)]
        source: Option<String>,
    },
    /// Register an overlooker from flags (POST /api/overlookers).
    Add(Box<AddOpts>),
    /// Remove an overlooker (DELETE).
    Rm {
        /// Overlooker id or name.
        name: String,
    },
    /// Enable an overlooker (arm it).
    Enable {
        /// Overlooker id or name.
        name: String,
    },
    /// Disable an overlooker (stop it cold, no redeploy).
    Disable {
        /// Overlooker id or name.
        name: String,
    },
    /// List every overlooker: name, enabled, trigger, program, last outcome.
    Ls,
    /// Fire a round now and print its outcome + summary.
    Run {
        /// Overlooker id or name.
        name: String,
        /// Simulate: every mutating action is stubbed and logged as "would do
        /// X", nothing is performed. Safe to repeat — the iteration primitive.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show an overlooker's round history (time, reason, outcome, summary).
    Runs {
        /// Overlooker id or name.
        name: String,
        /// How many recent rounds to show.
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Show the actions each recent round took (a verbose `runs`).
    Logs {
        /// Overlooker id or name.
        name: String,
        /// How many recent rounds to show.
        #[arg(long, default_value = "10")]
        limit: i64,
    },
}

/// Options for `loom overlooker add` — the flags build the trigger / scope /
/// program / capability set the REST `CreateOverlookerReq` takes.
#[derive(Args)]
struct AddOpts {
    /// The overlooker name (unique).
    name: String,
    /// Cron trigger: a standard 5-field crontab expression (e.g. "0 * * * *").
    #[arg(long, group = "trigger")]
    cron: Option<String>,
    /// Interval trigger sugar: a duration like `30m`, `2h`, `45s`.
    #[arg(long, group = "trigger")]
    every: Option<String>,
    /// Reactive trigger: fire on an event of this kind (e.g. `attention`).
    #[arg(long, group = "trigger")]
    on_event: Option<String>,
    /// With `--on-event`, narrow to a single level (e.g. `blocked`).
    #[arg(long)]
    level: Option<String>,
    /// Pin the overlooker to one repository (filters the trigger + scope).
    #[arg(long)]
    repo: Option<String>,
    /// Raw scope JSON, merged over the repo filter (e.g. '{"attention":"!ok"}').
    #[arg(long)]
    scope: Option<String>,
    /// The program: `builtin:<name>` (default `builtin:status`) or an absolute
    /// path to a custom program file.
    #[arg(long)]
    program: Option<String>,
    /// The stock-program judgement prompt; stored as `params.prompt`.
    #[arg(long)]
    prompt: Option<String>,
    /// Comma-separated capability set (default `observe,mark,escalate`). Drawn
    /// from observe, mark, escalate, nudge, interrupt, launch.
    #[arg(long, value_delimiter = ',')]
    capabilities: Option<Vec<String>>,
    /// Model tier for `run_agent` judgement calls (e.g. sonnet, opus).
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort for judgement calls.
    #[arg(long)]
    effort: Option<String>,
    /// Minimum gap between rounds, in seconds (a non-manual re-fire inside the
    /// gap is skipped).
    #[arg(long)]
    cooldown: Option<i64>,
}

/// Shared `launch` options, used by both `loom session launch` and the
/// deprecated top-level `loom launch` alias.
#[derive(Args)]
struct LaunchOpts {
    /// What the agent should do. Sets the branch goal and is fed to the agent as
    /// its first prompt. Multiple words are joined, so quoting is optional. Omit
    /// only when seeding from `--claim`/`--issue`/`--branch`.
    task: Vec<String>,
    /// Branch slug to create (`weaver/<name>`). Defaults to a slug derived from
    /// the task. Mutually exclusive with `--branch`.
    #[arg(long)]
    name: Option<String>,
    /// Agent to run: `claude` (the default), `shell` for a plain shell, or any
    /// other command. Optional — omit to use the configured `agent.default`
    /// (see `weaver config get agent.default`).
    #[arg(long)]
    agent: Option<String>,
    /// Branch to fork the new worktree from. Defaults to a freshly-fetched
    /// `origin/<default branch>` (the repo's mainline), so new work starts from
    /// the latest upstream rather than the launching checkout.
    #[arg(long)]
    base: Option<String>,
    /// One-line title shown on the dashboard. Defaults to a title derived from
    /// the task.
    #[arg(long)]
    title: Option<String>,
    /// Seed the task from a GitHub issue (by number, via the `gh` CLI): fills in
    /// title, goal, and description.
    #[arg(long)]
    issue: Option<i64>,
    /// Claim an existing weaver issue (by id) for this session: seeds the goal
    /// from it and moves it out of the repo backlog.
    #[arg(long)]
    claim: Option<i64>,
    /// Resume an existing branch rather than creating a new one. Mutually
    /// exclusive with `--name`.
    #[arg(long)]
    branch: Option<String>,
    /// Model tier: haiku, sonnet, opus, or fable. Omit to inherit the
    /// configured `agent.claude_args`.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort: low, medium, high, xhigh, or max. Omit to inherit the
    /// configured `agent.claude_args`.
    #[arg(long)]
    effort: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { addr } => {
            init_tracing();
            let addr = loom::endpoint::bind_addr(addr.as_deref());
            loom::server::run(&addr).await
        }
        Cmd::Status => cmd_status().await,
        Cmd::Start => cmd_start().await,
        Cmd::Stop => cmd_stop().await,
        Cmd::Restart => cmd_restart().await,
        Cmd::Session { cmd } => run_session(cmd).await,
        Cmd::Overlooker { cmd } => run_overlooker(cmd).await,
        Cmd::Launch(opts) => {
            eprintln!("note: `loom launch` is deprecated — use `loom session launch`");
            cmd_launch(opts.into()).await
        }
        Cmd::Ps => cmd_ps().await,
        Cmd::Issues { all, backlog } => cmd_issues(all, backlog).await,
        Cmd::Show { branch } => cmd_show(branch).await,
        Cmd::Attach { branch } => cmd_attach(branch).await,
        Cmd::Archive { branch } => cmd_archive(branch).await,
        Cmd::Adopt { branch } => cmd_adopt(branch).await,
        Cmd::Rm {
            branch,
            keep_branch,
        } => cmd_rm(branch, keep_branch).await,
        Cmd::Open => cmd_open().await,
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "loom", &mut std::io::stdout());
            Ok(())
        }
    }
}

/// Dispatch the `loom session <verb>` subcommands.
async fn run_session(cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::Launch(opts) => cmd_launch(opts.into()).await,
        SessionCmd::Poll { session } => cmd_session_poll(session).await,
        SessionCmd::Wait {
            session,
            timeout,
            interval,
            lifecycle_only,
        } => cmd_session_wait(session, timeout, interval.max(1), lifecycle_only).await,
        SessionCmd::Send {
            session,
            message,
            no_enter,
        } => cmd_session_send(session, message.join(" "), !no_enter).await,
        SessionCmd::Break { session } => cmd_session_break(session).await,
        SessionCmd::Preview { session, lines } => cmd_session_preview(session, lines).await,
    }
}

/// Dispatch the `loom overlooker <verb>` subcommands.
async fn run_overlooker(cmd: OverlookerCmd) -> Result<()> {
    match cmd {
        OverlookerCmd::New { name } => cmd_overlooker_new(name).await,
        OverlookerCmd::Programs { source } => cmd_overlooker_programs(source).await,
        OverlookerCmd::Add(opts) => cmd_overlooker_add(*opts).await,
        OverlookerCmd::Rm { name } => cmd_overlooker_rm(name).await,
        OverlookerCmd::Enable { name } => cmd_overlooker_set_enabled(name, true).await,
        OverlookerCmd::Disable { name } => cmd_overlooker_set_enabled(name, false).await,
        OverlookerCmd::Ls => cmd_overlooker_ls().await,
        OverlookerCmd::Run { name, dry_run } => cmd_overlooker_run(name, dry_run).await,
        OverlookerCmd::Runs { name, limit } => cmd_overlooker_runs(name, limit, false).await,
        OverlookerCmd::Logs { name, limit } => cmd_overlooker_runs(name, limit, true).await,
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("loom=info,weaver_core=info,tower_http=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Read a string field from a `SessionView`'s nested `branch` object.
fn branch_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get("branch")
        .and_then(|b| b.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The agent's resolved attention level from a `SessionView`'s `branch.tags` —
/// the value of the `attention` tag, or `ok` when it is absent (the calm state).
fn branch_attention(v: &Value) -> &str {
    v.get("branch")
        .and_then(|b| b.get("tags"))
        .and_then(Value::as_array)
        .and_then(|tags| {
            tags.iter()
                .find(|t| t.get("key").and_then(Value::as_str) == Some("attention"))
        })
        .and_then(|t| t.get("value").and_then(Value::as_str))
        .filter(|v| !v.is_empty())
        .unwrap_or("ok")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Percent-encode a query-string value (paths can contain spaces).
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Server lifecycle (status / start / stop / restart)
// ---------------------------------------------------------------------------

fn server_base() -> String {
    loom::endpoint::base_url()
}

async fn server_is_up(base: &str) -> bool {
    let url = format!("{base}/api/health");
    match reqwest::get(&url).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

fn format_uptime(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn uptime_secs(started_at: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    Some((chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds())
}

async fn wait_for_health(base: &str, want: bool, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if server_is_up(base).await == want {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn cmd_status() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("loom: not running");
        return Ok(());
    }
    match loom::server::read_state() {
        Some(state) => {
            print!(
                "loom: running at http://{}  (pid {})",
                state.addr, state.pid
            );
            match uptime_secs(&state.started_at) {
                Some(secs) => println!("  up {}", format_uptime(secs)),
                None => println!(),
            }
        }
        None => println!("loom: running at {base}  (no state file)"),
    }
    Ok(())
}

async fn cmd_start() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        println!("loom already running at {base}");
        return Ok(());
    }
    spawn_server().await
}

async fn spawn_server() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("locating the loom binary")?;
    let addr = loom::endpoint::bind_addr(None);
    let home = loom::db::weaver_home();
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let log_path = home.join("loom.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    let mut command = std::process::Command::new(&exe);
    command
        .args(["serve"])
        .arg("--addr")
        .arg(&addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    let child = command.spawn().context("spawning `loom serve`")?;
    drop(child);

    let base = format!("http://{addr}");
    if wait_for_health(&base, true, std::time::Duration::from_secs(10)).await {
        println!("loom started at {base}");
        Ok(())
    } else {
        bail!(
            "loom did not come up within 10s — check the log at {}",
            log_path.display()
        )
    }
}

async fn cmd_stop() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("loom is not running");
        return Ok(());
    }
    let state = loom::server::read_state().ok_or_else(|| {
        anyhow!(
            "loom is running but {} is missing or unreadable — stop it manually",
            loom::server::state_path().display()
        )
    })?;
    let status = std::process::Command::new("kill")
        .arg(state.pid.to_string())
        .status()
        .context("failed to run `kill`")?;
    if !status.success() {
        bail!(
            "`kill {}` failed — the process may already be gone",
            state.pid
        );
    }
    if wait_for_health(&base, false, std::time::Duration::from_secs(10)).await {
        println!("loom stopped (pid {})", state.pid);
        Ok(())
    } else {
        bail!("loom (pid {}) did not stop within 10s", state.pid)
    }
}

async fn cmd_restart() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        cmd_stop().await?;
    }
    spawn_server().await
}

// ---------------------------------------------------------------------------
// Session commands (HTTP)
// ---------------------------------------------------------------------------

/// Parsed launch inputs, after folding the positional task words into a single
/// `goal` string.
struct LaunchArgs {
    goal: String,
    name: Option<String>,
    agent: Option<String>,
    base: Option<String>,
    title: Option<String>,
    issue: Option<i64>,
    claim: Option<i64>,
    branch: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

impl From<LaunchOpts> for LaunchArgs {
    fn from(o: LaunchOpts) -> Self {
        LaunchArgs {
            goal: o.task.join(" "),
            name: o.name,
            agent: o.agent,
            base: o.base,
            title: o.title,
            issue: o.issue,
            claim: o.claim,
            branch: o.branch,
            model: o.model,
            effort: o.effort,
        }
    }
}

/// A bare `loom session launch` with nothing to work on — no task, no name, no title,
/// and nothing to pick up (`--claim`/`--issue`/`--branch`). Launching anyway
/// would spawn an agent with an empty goal that "starts unprompted", so we
/// stop and point the user at the useful forms instead.
fn launch_underspecified(a: &LaunchArgs) -> bool {
    a.goal.trim().is_empty()
        && a.name.is_none()
        && a.title.is_none()
        && a.issue.is_none()
        && a.claim.is_none()
        && a.branch.is_none()
}

const LAUNCH_HINT: &str = "nothing to do — give the agent a task or something to pick up:
  loom session launch \"<what the agent should do>\"   # the common case
  loom session launch --claim <id>                     # pick up a weaver issue
  loom session launch --issue <n>                      # seed from a GitHub issue
  loom session launch --branch <name>                  # resume an existing branch
  loom session launch --name <slug> --agent shell      # an empty named worktree (no task)
See `loom session launch --help` for all options.";

async fn cmd_launch(a: LaunchArgs) -> Result<()> {
    if launch_underspecified(&a) {
        bail!("{LAUNCH_HINT}");
    }
    let LaunchArgs {
        goal,
        name,
        agent,
        base,
        title,
        issue,
        claim,
        branch,
        model,
        effort,
    } = a;
    let client = client::default();
    let cwd = std::env::current_dir()?;
    // When an agent in a weaver session runs `loom session launch`,
    // `$WEAVER_BRANCH` is its own branch id — pass it so the tracking issue is
    // attributed to the launching (parent) agent. A human shell launch leaves it
    // unset.
    let parent_branch = std::env::var("WEAVER_BRANCH")
        .ok()
        .filter(|s| !s.is_empty());
    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": goal,
                "title": title,
                "cwd": cwd.display().to_string(),
                "base": base,
                "agent": agent,
                "name": name,
                "existing_branch": branch,
                "issue": issue,
                "claim_issue": claim,
                "parent_branch": parent_branch,
                "model": model,
                "effort": effort,
            }),
        )
        .await?;
    let id = str_field(&ws, "id");
    println!("launched session {id}  ({})", branch_str(&ws, "name"));
    println!("  title:  {}", branch_str(&ws, "title"));
    let g = branch_str(&ws, "goal");
    println!(
        "  goal:   {}",
        if g.is_empty() {
            "(none — agent started unprompted)"
        } else {
            g
        }
    );
    println!("  branch: {}", branch_str(&ws, "branch"));
    let model = str_field(&ws, "model");
    if !model.is_empty() {
        println!("  model:  {model}");
    }
    let effort = str_field(&ws, "effort");
    if !effort.is_empty() {
        println!("  effort: {effort}");
    }
    println!("  dir:    {}", str_field(&ws, "work_dir"));
    if let Some(n) = ws.get("tracking_issue").and_then(Value::as_i64) {
        // The handle the caller uses to follow this sub-tree: poll it with
        // `weaver issue show <n>`, or block on it with `weaver issue wait <n>`.
        println!("  track:  weaver issue #{n}  (weaver issue show {n} | wait {n})");
    }
    println!("  attach: loom attach {id}");
    Ok(())
}

/// Resolve a session view by key, surfacing a clearer error than a bare 404 when
/// the key matches no live session.
async fn fetch_session(client: &Client, key: &str) -> Result<Value> {
    client
        .get(&format!("/api/sessions/{key}"))
        .await
        .with_context(|| format!("no live session for '{key}'"))
}

/// One-line attention summary: the resolved level (the agent's `attention` tag,
/// `ok` when absent), plus its current-state message when set.
fn attention_summary(ws: &Value) -> String {
    let attention = branch_attention(ws);
    let message = branch_str(ws, "description");
    if message.is_empty() {
        attention.to_string()
    } else {
        format!("{attention} — {message}")
    }
}

/// `loom session poll` — a one-shot status read: lifecycle + attention.
async fn cmd_session_poll(key: String) -> Result<()> {
    let client = client::default();
    let ws = fetch_session(&client, &key).await?;
    println!(
        "session {}  ({})",
        str_field(&ws, "id"),
        branch_str(&ws, "name")
    );
    println!("  status:    {}", str_field(&ws, "status"));
    println!("  attention: {}", attention_summary(&ws));
    if let Some(n) = ws.get("tracking_issue").and_then(Value::as_i64) {
        println!("  track:     weaver issue #{n}");
    }
    println!("  activity:  {}", str_field(&ws, "last_activity_at"));
    Ok(())
}

/// `loom session wait` — block until the session finishes, is lost, or (unless
/// `lifecycle_only`) its agent raises attention. Mirrors `weaver issue wait`.
async fn cmd_session_wait(
    key: String,
    timeout: u64,
    interval: u64,
    lifecycle_only: bool,
) -> Result<()> {
    let client = client::default();
    // Short-circuit if the session is already in a wake state at call time.
    let ws = fetch_session(&client, &key).await?;
    if let Some(reason) = wake_reason(&ws, &key, lifecycle_only) {
        println!("{reason}");
        return Ok(());
    }
    println!(
        "waiting on {} ({}) — {}",
        key,
        branch_str(&ws, "name"),
        str_field(&ws, "status")
    );

    let interval = std::time::Duration::from_secs(interval);
    let deadline =
        (timeout > 0).then(|| std::time::Instant::now() + std::time::Duration::from_secs(timeout));
    loop {
        // Never nap past the deadline: a long `--interval` must not stretch a
        // short `--timeout`.
        let nap = match deadline {
            Some(d) => interval.min(d.saturating_duration_since(std::time::Instant::now())),
            None => interval,
        };
        tokio::time::sleep(nap).await;
        let ws = fetch_session(&client, &key).await?;
        if let Some(reason) = wake_reason(&ws, &key, lifecycle_only) {
            println!("{reason}");
            return Ok(());
        }
        // Timing out is a real "not done" outcome: report it as an error so the
        // process exits non-zero (callers branch on it).
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            bail!(
                "timed out after {timeout}s — session {key} still {}",
                str_field(&ws, "status")
            );
        }
    }
}

/// Why a `wait` should stop watching `ws`, or `None` to keep waiting: a terminal
/// or orphaned lifecycle, or — unless `lifecycle_only` — a raised attention.
fn wake_reason(ws: &Value, key: &str, lifecycle_only: bool) -> Option<String> {
    let status = str_field(ws, "status");
    if is_terminal_status(status) {
        return Some(format!("session {key} is {status} — finished"));
    }
    if status == "orphaned" {
        return Some(format!(
            "session {key} is orphaned — its tmux was lost (try `loom adopt {key}`)"
        ));
    }
    if !lifecycle_only && branch_attention(ws) != "ok" {
        return Some(format!(
            "session {key} needs you — {}",
            attention_summary(ws)
        ));
    }
    None
}

/// The terminal session lifecycle states (mirrors `session::is_terminal`).
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "done" | "error" | "archived")
}

/// `loom session send` — type a message into the agent's pane, submitting it
/// (Enter) unless `submit` is false.
async fn cmd_session_send(key: String, message: String, submit: bool) -> Result<()> {
    if message.trim().is_empty() {
        bail!("nothing to send — provide a message");
    }
    let client = client::default();
    client
        .post(
            &format!("/api/sessions/{key}/send"),
            json!({ "text": message, "submit": submit }),
        )
        .await?;
    println!(
        "sent to {key}{}",
        if submit { "" } else { " (not submitted)" }
    );
    Ok(())
}

/// `loom session break` — send Escape to interrupt the agent's current turn.
async fn cmd_session_break(key: String) -> Result<()> {
    let client = client::default();
    client
        .post(&format!("/api/sessions/{key}/interrupt"), json!({}))
        .await?;
    println!("sent break (Escape) to {key}");
    Ok(())
}

/// `loom session preview` — print the session's recent tmux screen.
async fn cmd_session_preview(key: String, lines: usize) -> Result<()> {
    let client = client::default();
    let res = client
        .get(&format!("/api/sessions/{key}/preview?lines={lines}"))
        .await?;
    print!("{}", str_field(&res, "screen"));
    // The capture is right-trimmed server-side; ensure a clean final newline.
    println!();
    Ok(())
}

async fn cmd_ps() -> Result<()> {
    let client = client::default();
    let list = client.get("/api/sessions").await?;
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no sessions — start one with `loom session launch \"<task>\"`");
        return Ok(());
    }
    println!(
        "{:<10}  {:<9}  {:<10}  {:<22}  TITLE",
        "ID", "STATUS", "ATTENTION", "NAME"
    );
    for ws in rows {
        println!(
            "{:<10}  {:<9}  {:<10}  {:<22}  {}",
            str_field(&ws, "id"),
            str_field(&ws, "status"),
            branch_attention(&ws),
            truncate(branch_str(&ws, "name"), 22),
            truncate(branch_str(&ws, "title"), 46),
        );
    }
    Ok(())
}

async fn cmd_issues(all: bool, backlog: bool) -> Result<()> {
    let client = client::default();
    let cwd = std::env::current_dir()?;
    let scope = if backlog { "backlog" } else { "repo" };
    let path = format!(
        "/api/repos/issues?cwd={}&all={all}&scope={scope}",
        encode_query(&cwd.display().to_string()),
    );
    let rows = client
        .get(&path)
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("(no issues)");
        return Ok(());
    }
    println!("{:<6}  {:<5}  {:<18}  TITLE", "ID", "STATE", "CLAIM");
    for i in rows {
        let id = i.get("id").and_then(Value::as_i64).unwrap_or(0);
        let state = if str_field(&i, "status") == "open" {
            "open"
        } else {
            "done"
        };
        let claim = i
            .get("claimed_branch")
            .and_then(Value::as_str)
            .map(|b| b.strip_prefix("weaver/").unwrap_or(b))
            .unwrap_or("(backlog)");
        println!(
            "{:<6}  {:<5}  {:<18}  {}",
            format!("#{id}"),
            state,
            truncate(claim, 18),
            truncate(str_field(&i, "title"), 50),
        );
    }
    Ok(())
}

async fn cmd_show(key: String) -> Result<()> {
    let client = client::default();
    let ws = client.get(&format!("/api/sessions/{key}")).await?;
    print_session(&ws);
    Ok(())
}

fn print_session(ws: &Value) {
    println!(
        "session {}  ({})",
        str_field(ws, "id"),
        branch_str(ws, "name")
    );
    println!("  title:    {}", branch_str(ws, "title"));
    println!("  status:   {}", str_field(ws, "status"));
    // Agent-declared attention level (the resolved `attention` tag) plus its
    // current-state message (the branch `description`), shown together — one
    // signal.
    let attention = branch_attention(ws);
    let message = branch_str(ws, "description");
    let attention = if message.is_empty() {
        attention.to_string()
    } else {
        format!("{attention} — {message}")
    };
    println!("  attention: {attention}");
    let goal = branch_str(ws, "goal");
    println!(
        "  goal:     {}",
        if goal.is_empty() { "(none)" } else { goal }
    );
    println!("  agent:    {}", str_field(ws, "agent_kind"));
    let model = str_field(ws, "model");
    if !model.is_empty() {
        println!("  model:    {model}");
    }
    let effort = str_field(ws, "effort");
    if !effort.is_empty() {
        println!("  effort:   {effort}");
    }
    println!(
        "  branch:   {} (base {})",
        branch_str(ws, "branch"),
        branch_str(ws, "base_branch")
    );
    println!("  work_dir: {}", str_field(ws, "work_dir"));
    println!("  session:  {}", str_field(ws, "tmux_session"));
    if let Some(repo) = ws.get("github_repo").and_then(Value::as_str) {
        if !repo.is_empty() {
            println!("  github:   {repo}");
        }
    }
    // The branch's PR snapshot, when loom has polled one (see `loom::github`).
    if let Some(gh) = ws.get("branch").and_then(|b| b.get("github")) {
        if let Some(url) = gh.get("pr_url").and_then(Value::as_str) {
            let number = gh.get("pr_number").and_then(Value::as_i64).unwrap_or(0);
            let state = gh
                .get("pr_state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let mut bits = vec![state];
            if let Some(review) = gh.get("review_decision").and_then(Value::as_str) {
                bits.push(review.to_lowercase().replace('_', " "));
            }
            if let Some(checks) = gh.get("checks").and_then(Value::as_str) {
                bits.push(format!("checks {checks}"));
            }
            let bits: Vec<String> = bits.into_iter().filter(|b| !b.is_empty()).collect();
            println!("  pr:       #{number} {url} ({})", bits.join(", "));
        }
    }
    println!("  activity: {}", str_field(ws, "last_activity_at"));
}

async fn cmd_attach(key: String) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let client = client::default();
    let ws = client.get(&format!("/api/sessions/{key}")).await?;
    let session = ws
        .get("tmux_session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("session has no tmux session"))?;
    let target = loom::tmux::exact(session);
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &target])
        .exec();
    Err(anyhow!("failed to exec tmux: {err}"))
}

async fn cmd_archive(key: String) -> Result<()> {
    let client = client::default();
    let res = client
        .post(&format!("/api/sessions/{key}/archive"), json!({}))
        .await?;
    println!(
        "archived {} (tmux + worktree removed; branch and history kept)",
        str_field(&res, "branch")
    );
    if let Some(warnings) = res.get("warnings").and_then(Value::as_array) {
        for w in warnings {
            if let Some(w) = w.as_str() {
                eprintln!("  warning: {w}");
            }
        }
    }
    Ok(())
}

async fn cmd_adopt(key: String) -> Result<()> {
    let client = client::default();
    let ws = client
        .post(&format!("/api/sessions/{key}/adopt"), json!({}))
        .await?;
    println!(
        "adopted session {}  ({})",
        str_field(&ws, "id"),
        branch_str(&ws, "name")
    );
    println!("  status:  {}", str_field(&ws, "status"));
    println!("  session: {}", str_field(&ws, "tmux_session"));
    println!("  attach:  loom attach {}", str_field(&ws, "id"));
    Ok(())
}

async fn cmd_rm(key: String, keep_branch: bool) -> Result<()> {
    let client = client::default();
    let path = format!("/api/sessions/{key}?keep_branch={keep_branch}");
    let res = client.delete(&path).await?;
    println!("removed session {key}");
    if let Some(warnings) = res.get("warnings").and_then(Value::as_array) {
        for w in warnings {
            if let Some(w) = w.as_str() {
                eprintln!("  warning: {w}");
            }
        }
    }
    Ok(())
}

async fn cmd_open() -> Result<()> {
    let client = client::default();
    let url = client.base().to_string();
    println!("opening {url}");
    if std::process::Command::new("xdg-open")
        .arg(&url)
        .status()
        .is_err()
    {
        println!("open it manually: {url}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Overlooker commands (the operator + authoring surface)
// ---------------------------------------------------------------------------

/// The starter program a `loom overlooker new` scaffolds: a small, runnable
/// template against the `weaver_loom` API layer and the program contract the
/// engine speaks — the same shape the builtin scripts implement
/// (`loom overlooker programs --source <name>` prints one as a fuller
/// example). Plain `replace` rather than `format!`, so the template's literal
/// braces (JSON, f-strings) stay readable.
fn scaffold_template(name: &str) -> String {
    const TEMPLATE: &str = r##"# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""__NAME__ — a weaver overlooker program.

The engine runs this as a subprocess with WEAVER_API (the loom REST base URL)
and WEAVER_OVERLOOKER (the round config JSON) set; `weaver_loom` is on
PYTHONPATH. `Round.finish` prints the result the engine reads from stdout.

Register:   loom overlooker add __NAME__ --program __PATH__ --every 15m
Try it:     loom overlooker run __NAME__ --dry-run
"""

from weaver_loom import Round


def main():
    rnd = Round()
    for session in rnd.sessions():
        # Decide per session and record findings, e.g.:
        #     rnd.would("mark", session=session["id"], note="one line on why")
        pass
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} finding(s)")


if __name__ == "__main__":
    main()
"##;
    TEMPLATE
        .replace("__NAME__", name)
        .replace("__PATH__", &overlooker_path(name).display().to_string())
}

/// The conventional path for an overlooker's program file:
/// `~/.weaver/overlookers/<name>.py`.
fn overlooker_path(name: &str) -> std::path::PathBuf {
    loom::db::weaver_home()
        .join("overlookers")
        .join(format!("{name}.py"))
}

/// `loom overlooker new` — scaffold a starter program file and print its path.
/// A local file-convention command: it touches no server (T8 file convention),
/// so it works before the Python binding exists.
async fn cmd_overlooker_new(name: String) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("name must not be empty");
    }
    let dir = loom::db::weaver_home().join("overlookers");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = overlooker_path(name);
    if path.exists() {
        bail!(
            "{} already exists — edit it, or pick another name",
            path.display()
        );
    }
    std::fs::write(&path, scaffold_template(name))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("scaffolded {}", path.display());
    println!("  edit it, then register:");
    println!(
        "    loom overlooker add {name} --program {} --cron \"0 * * * *\"",
        path.display()
    );
    Ok(())
}

/// `loom overlooker programs` — list the builtin programs that ship with loom
/// (the registry the panel offers), or print one program's script source with
/// `--source` as a working example to start a custom program from.
async fn cmd_overlooker_programs(source: Option<String>) -> Result<()> {
    let client = client::default();
    let rows = client
        .get("/api/overlookers/programs")
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(want) = source {
        let row = rows.iter().find(|p| str_field(p, "program") == want);
        let Some(row) = row else {
            bail!("no builtin program '{want}' — `loom overlooker programs` lists them");
        };
        print!("{}", str_field(row, "source"));
        return Ok(());
    }
    println!("{:<26}  TITLE", "PROGRAM");
    for p in rows {
        println!(
            "{:<26}  {}",
            str_field(&p, "program"),
            str_field(&p, "title"),
        );
    }
    Ok(())
}

/// Build the trigger JSON from the `add` flags. clap's `group = "trigger"`
/// already makes cron/every/on-event mutually exclusive; `repo` is folded in
/// when present. An empty trigger (`{}`) is a valid, never-firing default.
fn build_trigger(opts: &AddOpts) -> Value {
    let mut t = serde_json::Map::new();
    if let Some(cron) = &opts.cron {
        t.insert("cron".into(), json!(cron));
    }
    if let Some(every) = &opts.every {
        t.insert("every".into(), json!(every));
    }
    if let Some(event) = &opts.on_event {
        t.insert("event".into(), json!(event));
        if let Some(level) = &opts.level {
            t.insert("level".into(), json!(level));
        }
    }
    if let Some(repo) = &opts.repo {
        t.insert("repo".into(), json!(repo));
    }
    Value::Object(t)
}

/// Build the scope JSON: the explicit `--scope` JSON if given (parsed), with the
/// `--repo` filter folded in so a repo-pinned overlooker only surveys its repo.
fn build_scope(opts: &AddOpts) -> Result<Value> {
    let mut scope = match &opts.scope {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .with_context(|| format!("--scope is not valid JSON: {raw}"))?,
        None => json!({}),
    };
    if let Some(repo) = &opts.repo {
        if let Some(obj) = scope.as_object_mut() {
            obj.entry("repo").or_insert_with(|| json!(repo));
        }
    }
    Ok(scope)
}

/// `loom overlooker add` — register an overlooker via POST /api/overlookers.
async fn cmd_overlooker_add(opts: AddOpts) -> Result<()> {
    let client = client::default();
    let trigger = build_trigger(&opts);
    let scope = build_scope(&opts)?;
    let params = opts
        .prompt
        .as_ref()
        .map(|p| json!({ "prompt": p }))
        .unwrap_or_else(|| json!({}));

    let mut body = serde_json::Map::new();
    body.insert("name".into(), json!(opts.name));
    body.insert("trigger".into(), trigger);
    body.insert("scope".into(), scope);
    body.insert("params".into(), params);
    if let Some(program) = &opts.program {
        body.insert("program".into(), json!(program));
    }
    if let Some(caps) = &opts.capabilities {
        body.insert("capabilities".into(), json!(caps));
    }
    if let Some(model) = &opts.model {
        body.insert("model".into(), json!(model));
    }
    if let Some(effort) = &opts.effort {
        body.insert("effort".into(), json!(effort));
    }
    if let Some(cooldown) = opts.cooldown {
        body.insert("cooldown_secs".into(), json!(cooldown));
    }

    let o = client.post("/api/overlookers", Value::Object(body)).await?;
    println!(
        "registered overlooker {}  ({})",
        str_field(&o, "name"),
        str_field(&o, "id")
    );
    println!("  trigger: {}", trigger_summary(&o));
    println!("  program: {}", str_field(&o, "program"));
    println!("  caps:    {}", capabilities_summary(&o));
    println!(
        "  enabled: no — arm it with `loom overlooker enable {}`",
        opts.name
    );
    Ok(())
}

/// `loom overlooker rm` — delete an overlooker.
async fn cmd_overlooker_rm(name: String) -> Result<()> {
    let client = client::default();
    client.delete(&format!("/api/overlookers/{name}")).await?;
    println!("removed overlooker {name}");
    Ok(())
}

/// `loom overlooker enable|disable` — PATCH the `enabled` toggle.
async fn cmd_overlooker_set_enabled(name: String, enabled: bool) -> Result<()> {
    let client = client::default();
    let o = client
        .patch(
            &format!("/api/overlookers/{name}"),
            json!({ "enabled": enabled }),
        )
        .await?;
    println!(
        "{} overlooker {}",
        if enabled { "enabled" } else { "disabled" },
        str_field(&o, "name")
    );
    Ok(())
}

/// `loom overlooker ls` — a table of every overlooker.
async fn cmd_overlooker_ls() -> Result<()> {
    let client = client::default();
    let rows = client
        .get("/api/overlookers")
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("no overlookers — scaffold one with `loom overlooker new <name>`");
        return Ok(());
    }
    println!(
        "{:<18}  {:<8}  {:<22}  {:<18}  LAST",
        "NAME", "ENABLED", "TRIGGER", "PROGRAM"
    );
    for o in rows {
        let enabled = if o.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
            "yes"
        } else {
            "no"
        };
        let last = o.get("last_outcome").and_then(Value::as_str).unwrap_or("—");
        println!(
            "{:<18}  {:<8}  {:<22}  {:<18}  {}",
            truncate(str_field(&o, "name"), 18),
            enabled,
            truncate(&trigger_summary(&o), 22),
            truncate(str_field(&o, "program"), 18),
            last,
        );
    }
    Ok(())
}

/// `loom overlooker run` — fire a round now and print outcome + summary.
async fn cmd_overlooker_run(name: String, dry_run: bool) -> Result<()> {
    let client = client::default();
    let res = client
        .post(
            &format!("/api/overlookers/{name}/run"),
            json!({ "dry_run": dry_run }),
        )
        .await?;
    let outcome = str_field(&res, "outcome");
    let summary = str_field(&res, "summary");
    let kind = if dry_run { "dry run" } else { "run" };
    println!("{name} {kind}: {outcome}");
    if !summary.is_empty() {
        println!("  {summary}");
    }
    Ok(())
}

/// `loom overlooker runs` / `logs` — the round history. `verbose` (the `logs`
/// alias) also prints each round's actions.
async fn cmd_overlooker_runs(name: String, limit: i64, verbose: bool) -> Result<()> {
    let client = client::default();
    let rows = client
        .get(&format!("/api/overlookers/{name}/runs?limit={limit}"))
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("no rounds yet for {name} — fire one with `loom overlooker run {name}`");
        return Ok(());
    }
    if !verbose {
        println!(
            "{:<24}  {:<14}  {:<8}  SUMMARY",
            "WHEN", "REASON", "OUTCOME"
        );
    }
    for r in &rows {
        let when = str_field(r, "started_at");
        let reason = str_field(r, "trigger_reason");
        let outcome = str_field(r, "outcome");
        let summary = str_field(r, "summary");
        if verbose {
            println!("{when}  [{reason}]  {outcome}");
            if !summary.is_empty() {
                println!("  {summary}");
            }
            if let Some(actions) = r.get("actions").and_then(Value::as_array) {
                for a in actions {
                    println!("    - {}", action_summary(a));
                }
            }
        } else {
            println!(
                "{:<24}  {:<14}  {:<8}  {}",
                when,
                truncate(reason, 14),
                outcome,
                truncate(summary, 60),
            );
        }
    }
    Ok(())
}

/// A one-line summary of a round action (a mark / nudge / would-do entry).
fn action_summary(a: &Value) -> String {
    // A mutating action carries `action`; a dry-run stub carries `would`.
    let verb = a
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            a.get("would")
                .and_then(Value::as_str)
                .map(|w| format!("would {w}"))
        })
        .unwrap_or_else(|| "?".to_string());
    let session = a.get("session").and_then(Value::as_str).unwrap_or("");
    let detail = a
        .get("level")
        .and_then(Value::as_str)
        .map(|l| {
            let note = a.get("note").and_then(Value::as_str).unwrap_or("");
            if note.is_empty() {
                l.to_string()
            } else {
                format!("{l} — {note}")
            }
        })
        .or_else(|| a.get("text").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    if detail.is_empty() {
        format!("{verb} {session}")
    } else {
        format!("{verb} {session}: {detail}")
    }
}

/// A compact human summary of an `OverlookerView`'s parsed `trigger` object.
fn trigger_summary(o: &Value) -> String {
    let Some(t) = o.get("trigger") else {
        return "—".to_string();
    };
    if let Some(cron) = t.get("cron").and_then(Value::as_str) {
        return format!("cron {cron}");
    }
    if let Some(every) = t.get("every").and_then(Value::as_str) {
        return format!("every {every}");
    }
    if let Some(event) = t.get("event").and_then(Value::as_str) {
        return match t.get("level").and_then(Value::as_str) {
            Some(level) => format!("on {event}={level}"),
            None => format!("on {event}"),
        };
    }
    "—".to_string()
}

/// The granted capability set, comma-joined, for an `OverlookerView`.
fn capabilities_summary(o: &Value) -> String {
    o.get("capabilities")
        .and_then(Value::as_array)
        .map(|caps| {
            caps.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "observe".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_picks_a_sensible_granularity() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(-5), "0s");
        assert_eq!(format_uptime(42), "42s");
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(3_600), "1h 0m");
        assert_eq!(format_uptime(3_661), "1h 1m");
        assert_eq!(format_uptime(90_061), "1d 1h 1m");
    }

    #[test]
    fn truncate_respects_the_max_length() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long string", 6), "a ver…");
    }

    /// clap's own consistency check over the full command tree — catches a
    /// malformed arg/subcommand (e.g. the nested `session` group) at test time
    /// rather than on first run.
    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The scaffold must honor the contract it documents — at minimum, be
    /// valid Python with the placeholders filled in. Skips without `python3`
    /// (the same degradation the engine applies).
    #[test]
    fn scaffold_template_is_valid_python() {
        if !loom::builtins::python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let rendered = scaffold_template("test-watch");
        assert!(rendered.contains("test-watch"), "the name is filled in");
        assert!(!rendered.contains("__NAME__"), "no placeholder survives");
        assert!(!rendered.contains("__PATH__"), "no placeholder survives");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-watch.py");
        std::fs::write(&path, rendered).unwrap();
        let out = std::process::Command::new("python3")
            .args(["-m", "py_compile"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the scaffold does not compile: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn terminal_statuses_match_the_session_model() {
        for s in ["done", "error", "archived"] {
            assert!(is_terminal_status(s), "{s} should be terminal");
        }
        for s in ["created", "launching", "running", "orphaned"] {
            assert!(!is_terminal_status(s), "{s} should not be terminal");
        }
    }

    fn view(status: &str, attention: &str, description: &str) -> Value {
        // `ok` is the calm, tag-less state; any other level is the `attention`
        // tag's value, mirroring the wire `branch.tags` shape.
        let tags = if attention == "ok" {
            json!([])
        } else {
            json!([{ "key": "attention", "value": attention }])
        };
        json!({
            "status": status,
            "branch": { "tags": tags, "description": description },
        })
    }

    #[test]
    fn wake_reason_fires_on_terminal_orphan_and_attention() {
        // A running, ok session keeps the wait blocked.
        assert!(wake_reason(&view("running", "ok", ""), "s", false).is_none());

        // Terminal and orphaned lifecycles always wake.
        assert!(wake_reason(&view("done", "ok", ""), "s", false)
            .unwrap()
            .contains("finished"));
        assert!(wake_reason(&view("orphaned", "ok", ""), "s", false)
            .unwrap()
            .contains("orphaned"));

        // A raised attention wakes — and carries the message — unless lifecycle_only.
        let needs = wake_reason(&view("running", "blocked", "build broken"), "s", false).unwrap();
        assert!(needs.contains("needs you") && needs.contains("build broken"));
        assert!(wake_reason(&view("running", "blocked", "build broken"), "s", true).is_none());
    }

    fn empty_launch() -> LaunchArgs {
        LaunchArgs {
            goal: String::new(),
            name: None,
            agent: None,
            base: None,
            title: None,
            issue: None,
            claim: None,
            branch: None,
            model: None,
            effort: None,
        }
    }

    #[test]
    fn bare_launch_is_underspecified() {
        // `loom session launch` with nothing, or only an agent/model/effort/base
        // selector, has no actual task to run.
        assert!(launch_underspecified(&empty_launch()));
        let only_agent = LaunchArgs {
            agent: Some("shell".into()),
            base: Some("main".into()),
            model: Some("opus".into()),
            ..empty_launch()
        };
        assert!(launch_underspecified(&only_agent));
    }

    #[test]
    fn anything_to_work_on_is_enough() {
        let cases = [
            LaunchArgs {
                goal: "fix the bug".into(),
                ..empty_launch()
            },
            LaunchArgs {
                name: Some("scratch".into()),
                ..empty_launch()
            },
            LaunchArgs {
                title: Some("A title".into()),
                ..empty_launch()
            },
            LaunchArgs {
                issue: Some(42),
                ..empty_launch()
            },
            LaunchArgs {
                claim: Some(7),
                ..empty_launch()
            },
            LaunchArgs {
                branch: Some("weaver/foo".into()),
                ..empty_launch()
            },
        ];
        for a in cases {
            assert!(!launch_underspecified(&a));
        }
        // Whitespace-only task words still count as empty.
        assert!(launch_underspecified(&LaunchArgs {
            goal: "   ".into(),
            ..empty_launch()
        }));
    }

    fn empty_add(name: &str) -> AddOpts {
        AddOpts {
            name: name.to_string(),
            cron: None,
            every: None,
            on_event: None,
            level: None,
            repo: None,
            scope: None,
            program: None,
            prompt: None,
            capabilities: None,
            model: None,
            effort: None,
            cooldown: None,
        }
    }

    /// The scaffolded program carries the pieces an author starts from: the
    /// PEP 723 block (uv-runnable), a docstring documenting the contract, and
    /// the `weaver_loom` round context.
    #[test]
    fn scaffold_template_is_well_formed() {
        let out = scaffold_template("test-watch");
        assert!(out.starts_with("# /// script"), "PEP 723 block leads");
        // The docstring opens with exactly three quotes (a malformed `""` would
        // be the most likely raw-string bug).
        assert!(out.contains("\"\"\"test-watch — "));
        // It documents the program contract and uses the API layer.
        assert!(out.contains("WEAVER_OVERLOOKER"));
        assert!(out.contains("from weaver_loom import Round"));
        assert!(out.contains("loom overlooker add test-watch"));
    }

    /// `loom overlooker new` writes the file under `~/.weaver/overlookers/`,
    /// creating the dir, and refuses to clobber an existing one.
    #[tokio::test]
    #[serial_test::serial]
    async fn overlooker_new_scaffolds_under_weaver_home() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        cmd_overlooker_new("scaffolded".to_string()).await.unwrap();
        let path = home.path().join("overlookers").join("scaffolded.py");
        assert!(path.exists(), "the program file was written");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("\"\"\"scaffolded — "));
        // A second `new` of the same name refuses rather than clobbering.
        assert!(cmd_overlooker_new("scaffolded".to_string()).await.is_err());
        std::env::remove_var("WEAVER_HOME");
    }

    #[test]
    fn build_trigger_maps_each_flag() {
        let cron = build_trigger(&AddOpts {
            cron: Some("0 * * * *".into()),
            ..empty_add("a")
        });
        assert_eq!(cron, json!({ "cron": "0 * * * *" }));

        let every = build_trigger(&AddOpts {
            every: Some("30m".into()),
            repo: Some("/r".into()),
            ..empty_add("a")
        });
        assert_eq!(every, json!({ "every": "30m", "repo": "/r" }));

        let event = build_trigger(&AddOpts {
            on_event: Some("attention".into()),
            level: Some("blocked".into()),
            ..empty_add("a")
        });
        assert_eq!(event, json!({ "event": "attention", "level": "blocked" }));
    }

    #[test]
    fn build_scope_folds_in_the_repo_filter() {
        // `--repo` alone becomes a repo-scoped query.
        let s = build_scope(&AddOpts {
            repo: Some("/r".into()),
            ..empty_add("a")
        })
        .unwrap();
        assert_eq!(s, json!({ "repo": "/r" }));

        // An explicit `--scope` is merged with the repo filter, not clobbered.
        let s = build_scope(&AddOpts {
            scope: Some(r#"{"attention":"!ok"}"#.into()),
            repo: Some("/r".into()),
            ..empty_add("a")
        })
        .unwrap();
        assert_eq!(s, json!({ "attention": "!ok", "repo": "/r" }));

        // Bad scope JSON is an error.
        assert!(build_scope(&AddOpts {
            scope: Some("not json".into()),
            ..empty_add("a")
        })
        .is_err());
    }

    #[test]
    fn trigger_summary_reads_each_shape() {
        let cron = json!({ "trigger": { "cron": "0 * * * *" } });
        assert_eq!(trigger_summary(&cron), "cron 0 * * * *");
        let every = json!({ "trigger": { "every": "30m" } });
        assert_eq!(trigger_summary(&every), "every 30m");
        let event = json!({ "trigger": { "event": "attention", "level": "blocked" } });
        assert_eq!(trigger_summary(&event), "on attention=blocked");
        let empty = json!({ "trigger": {} });
        assert_eq!(trigger_summary(&empty), "—");
    }

    #[test]
    fn action_summary_renders_marks_nudges_and_would_dos() {
        let mark =
            json!({ "action": "mark", "session": "s1", "level": "blocked", "note": "stuck" });
        assert_eq!(action_summary(&mark), "mark s1: blocked — stuck");
        let would = json!({ "would": "mark", "session": "s1", "level": "ok" });
        assert_eq!(action_summary(&would), "would mark s1: ok");
        let nudge = json!({ "action": "nudge", "session": "s1", "text": "try again" });
        assert_eq!(action_summary(&nudge), "nudge s1: try again");
    }
}
