//! weaver CLI — a thin client over the local weaver server, plus `serve`.

use anyhow::{anyhow, bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};
use weaver::client::Client;

#[derive(Parser)]
#[command(name = "weaver", version, about = "Manage concurrent agent workstreams")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the weaver server.
    Serve {
        /// Address to bind. Defaults to $WEAVER_API, then 127.0.0.1:7878.
        #[arg(long)]
        addr: Option<String>,
    },
    /// Create a new workspace: worktree + tmux session + agent.
    New {
        /// What the agent should do. Optional — omit to start it unprompted.
        goal: Vec<String>,
        /// Human-readable title (derived from the goal when omitted).
        #[arg(long)]
        title: Option<String>,
        /// Base branch to fork from (defaults to the repo's current branch).
        #[arg(long)]
        base: Option<String>,
        /// Agent to launch: claude (default), shell, or a custom command.
        #[arg(long)]
        agent: Option<String>,
        /// Explicit name / branch slug (derived from the title when omitted).
        #[arg(long)]
        name: Option<String>,
        /// GitHub issue number to link and seed the workspace from.
        #[arg(long)]
        issue: Option<i64>,
    },
    /// List all workspaces.
    Ls,
    /// Show one workspace in detail (or all when no id given).
    Status { id: Option<String> },
    /// Attach your terminal to a workspace's tmux session.
    Attach { id: String },
    /// Print a workspace's worktree directory (e.g. `cd "$(weaver path <id>)"`).
    Path { id: String },
    /// Send a line of text to a workspace's agent.
    Send { id: String, text: Vec<String> },
    /// Interrupt a workspace's agent (sends Esc, like pressing it in the TUI).
    Interrupt { id: String },
    /// Force a fresh summary of a workspace now.
    Summary { id: String },
    /// Merge a workspace's branch into its base branch.
    Merge { id: String },
    /// Recreate the tmux session for an orphaned workspace and resume its agent.
    Adopt { id: String },
    /// Remove a workspace (worktree + tmux session).
    Rm {
        id: String,
        /// Keep the git branch instead of deleting it.
        #[arg(long)]
        keep_branch: bool,
    },
    /// Open the web UI in a browser.
    Open,
    /// Print the goal of the current workspace (run inside a worktree).
    Goal,
    /// Set the description of the current workspace (run inside a worktree).
    Description { text: Vec<String> },
    /// Append a note to the current workspace (run inside a worktree).
    Note { text: Vec<String> },
    /// Report agent status — invoked by Claude Code hooks.
    Hook {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        event: String,
    },
    /// Get, set, or list configuration.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Manage the weaver server process.
    Server {
        #[command(subcommand)]
        cmd: ServerCmd,
    },
    /// Generate shell completions.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
    List,
}

#[derive(Subcommand)]
enum ServerCmd {
    /// Report whether the server is running.
    Status,
    /// Start the server in the background if it is not already running.
    Start,
    /// Stop a running server gracefully.
    Stop,
    /// Stop the server (if running) and start it again.
    Restart,
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
            let addr = weaver::endpoint::bind_addr(addr.as_deref());
            weaver::server::run(&addr).await
        }
        Cmd::New {
            goal,
            title,
            base,
            agent,
            name,
            issue,
        } => cmd_new(goal.join(" "), title, base, agent, name, issue).await,
        Cmd::Ls => cmd_ls().await,
        Cmd::Status { id } => cmd_status(id).await,
        Cmd::Attach { id } => cmd_attach(id).await,
        Cmd::Path { id } => cmd_path(id).await,
        Cmd::Send { id, text } => cmd_send(id, text.join(" ")).await,
        Cmd::Interrupt { id } => cmd_interrupt(id).await,
        Cmd::Summary { id } => cmd_summary(id).await,
        Cmd::Merge { id } => cmd_merge(id).await,
        Cmd::Adopt { id } => cmd_adopt(id).await,
        Cmd::Rm { id, keep_branch } => cmd_rm(id, keep_branch).await,
        Cmd::Open => cmd_open().await,
        Cmd::Goal => cmd_goal().await,
        Cmd::Description { text } => cmd_description(text.join(" ")).await,
        Cmd::Note { text } => cmd_note(text.join(" ")).await,
        Cmd::Hook { workspace, event } => cmd_hook(workspace, event).await,
        Cmd::Config { cmd } => cmd_config(cmd).await,
        Cmd::Server { cmd } => cmd_server(cmd).await,
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "weaver", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("weaver=info,tower_http=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
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

// ---------------------------------------------------------------------------
// Management commands
// ---------------------------------------------------------------------------

async fn cmd_new(
    goal: String,
    title: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    issue: Option<i64>,
) -> Result<()> {
    let client = Client::new();
    let cwd = std::env::current_dir()?;
    let ws = client
        .post(
            "/api/workspaces",
            json!({
                "goal": goal,
                "title": title,
                "cwd": cwd.display().to_string(),
                "base": base,
                "agent": agent,
                "name": name,
                "issue": issue,
            }),
        )
        .await?;
    let id = str_field(&ws, "id");
    println!("created workspace {id}  ({})", str_field(&ws, "name"));
    println!("  title:  {}", str_field(&ws, "title"));
    let goal = str_field(&ws, "goal");
    println!(
        "  goal:   {}",
        if goal.is_empty() {
            "(none — agent started unprompted)"
        } else {
            goal
        }
    );
    println!("  branch: {}", str_field(&ws, "branch"));
    println!("  dir:    {}", str_field(&ws, "work_dir"));
    println!("  attach: weaver attach {id}");
    Ok(())
}

async fn cmd_ls() -> Result<()> {
    let client = Client::new();
    let list = client.get("/api/workspaces").await?;
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no workspaces — create one with `weaver new \"<goal>\"`");
        return Ok(());
    }
    println!("{:<10}  {:<9}  {:<24}  TITLE", "ID", "STATUS", "NAME");
    for ws in rows {
        println!(
            "{:<10}  {:<9}  {:<24}  {}",
            str_field(&ws, "id"),
            str_field(&ws, "status"),
            truncate(str_field(&ws, "name"), 24),
            truncate(str_field(&ws, "title"), 46),
        );
    }
    Ok(())
}

async fn cmd_status(id: Option<String>) -> Result<()> {
    let Some(id) = id else {
        return cmd_ls().await;
    };
    let client = Client::new();
    let ws = client.get(&format!("/api/workspaces/{id}")).await?;
    println!("workspace {}  ({})", str_field(&ws, "id"), str_field(&ws, "name"));
    println!("  title:    {}", str_field(&ws, "title"));
    println!("  status:   {}", str_field(&ws, "status"));
    let goal = str_field(&ws, "goal");
    println!("  goal:     {}", if goal.is_empty() { "(none)" } else { goal });
    let description = str_field(&ws, "description");
    if !description.is_empty() {
        println!("  summary:  {description}");
    }
    println!("  agent:    {}", str_field(&ws, "agent_kind"));
    println!("  branch:   {} (base {})", str_field(&ws, "branch"), str_field(&ws, "base_branch"));
    println!("  work_dir: {}", str_field(&ws, "work_dir"));
    println!("  session:  {}", str_field(&ws, "tmux_session"));
    if let Some(issue) = ws.get("github_issue").and_then(Value::as_i64) {
        println!("  github:   {} #{issue}", str_field(&ws, "github_repo"));
    }
    println!("  activity: {}", str_field(&ws, "last_activity_at"));
    let prompt = str_field(&ws, "pending_prompt");
    if !prompt.is_empty() {
        println!("  waiting on:");
        for line in prompt.lines() {
            println!("    {line}");
        }
    }
    Ok(())
}

async fn cmd_attach(id: String) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let client = Client::new();
    let ws = client.get(&format!("/api/workspaces/{id}")).await?;
    let session = ws
        .get("tmux_session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace has no tmux session"))?;
    // Replace this process with `tmux attach`.
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", session])
        .exec();
    Err(anyhow!("failed to exec tmux: {err}"))
}

async fn cmd_path(id: String) -> Result<()> {
    let client = Client::new();
    let ws = client.get(&format!("/api/workspaces/{id}")).await?;
    println!("{}", str_field(&ws, "work_dir"));
    Ok(())
}

async fn cmd_send(id: String, text: String) -> Result<()> {
    if text.is_empty() {
        bail!("nothing to send");
    }
    let client = Client::new();
    client
        .post(&format!("/api/workspaces/{id}/send"), json!({ "text": text }))
        .await?;
    println!("sent");
    Ok(())
}

async fn cmd_interrupt(id: String) -> Result<()> {
    let client = Client::new();
    client
        .post(&format!("/api/workspaces/{id}/interrupt"), json!({}))
        .await?;
    println!("interrupt sent");
    Ok(())
}

async fn cmd_summary(id: String) -> Result<()> {
    let client = Client::new();
    println!("summarizing… (this runs a headless agent and may take a moment)");
    let res = client
        .post(&format!("/api/workspaces/{id}/summarize"), json!({}))
        .await?;
    println!("{}", str_field(&res, "description"));
    Ok(())
}

async fn cmd_merge(id: String) -> Result<()> {
    let client = Client::new();
    let res = client
        .post(&format!("/api/workspaces/{id}/merge"), json!({}))
        .await?;
    println!("merged {}", str_field(&res, "branch"));
    let output = str_field(&res, "output");
    if !output.is_empty() {
        println!("{output}");
    }
    Ok(())
}

async fn cmd_adopt(id: String) -> Result<()> {
    let client = Client::new();
    let ws = client
        .post(&format!("/api/workspaces/{id}/adopt"), json!({}))
        .await?;
    println!(
        "adopted workspace {}  ({})",
        str_field(&ws, "id"),
        str_field(&ws, "name")
    );
    println!("  status:  {}", str_field(&ws, "status"));
    println!("  session: {}", str_field(&ws, "tmux_session"));
    println!("  attach:  weaver attach {}", str_field(&ws, "id"));
    Ok(())
}

async fn cmd_rm(id: String, keep_branch: bool) -> Result<()> {
    let client = Client::new();
    let path = format!("/api/workspaces/{id}?keep_branch={keep_branch}");
    let res = client.delete(&path).await?;
    println!("removed workspace {id}");
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
    let client = Client::new();
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
// Agent-facing commands (run inside a worktree)
// ---------------------------------------------------------------------------

/// Resolve the workspace the current process is operating in.
async fn current_workspace(client: &Client) -> Result<Value> {
    if let Ok(id) = std::env::var("WEAVER_WORKSPACE") {
        if !id.is_empty() {
            return client.get(&format!("/api/workspaces/{id}")).await;
        }
    }
    let cwd = std::env::current_dir()?.canonicalize()?;
    let list = client.get("/api/workspaces").await?;
    for ws in list.as_array().cloned().unwrap_or_default() {
        if let Some(work_dir) = ws.get("work_dir").and_then(Value::as_str) {
            if let Ok(work_dir) = std::path::Path::new(work_dir).canonicalize() {
                if cwd.starts_with(&work_dir) {
                    return Ok(ws);
                }
            }
        }
    }
    bail!("not inside a weaver workspace (set WEAVER_WORKSPACE or run from a worktree)")
}

async fn cmd_goal() -> Result<()> {
    let client = Client::new();
    let ws = current_workspace(&client).await?;
    println!("{}", str_field(&ws, "goal"));
    Ok(())
}

async fn cmd_description(text: String) -> Result<()> {
    if text.is_empty() {
        bail!("description text is required");
    }
    let client = Client::new();
    let ws = current_workspace(&client).await?;
    let id = str_field(&ws, "id");
    client
        .patch(
            &format!("/api/workspaces/{id}"),
            json!({ "description": text }),
        )
        .await?;
    println!("description updated");
    Ok(())
}

async fn cmd_note(text: String) -> Result<()> {
    if text.is_empty() {
        bail!("note text is required");
    }
    let client = Client::new();
    let ws = current_workspace(&client).await?;
    let id = str_field(&ws, "id");
    client
        .post(&format!("/api/workspaces/{id}/note"), json!({ "text": text }))
        .await?;
    println!("noted");
    Ok(())
}

async fn cmd_hook(workspace: String, event: String) -> Result<()> {
    // Hooks must never disrupt the agent: swallow all errors.
    // SessionStart prints the workspace primer — Claude Code injects a
    // SessionStart hook's stdout into the session as additional context — and
    // reports the workspace as `working`.
    let status = if event == "session-start" {
        print!("{}", weaver::agent::session_primer());
        "working"
    } else {
        event.as_str()
    };
    let client = Client::new();
    let _ = client
        .post("/api/hook", json!({ "workspace": workspace, "event": status }))
        .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

async fn cmd_config(cmd: ConfigCmd) -> Result<()> {
    let client = Client::new();
    match cmd {
        ConfigCmd::List => {
            let settings = client.get("/api/settings").await?;
            if let Some(map) = settings.as_object() {
                if map.is_empty() {
                    println!("no settings");
                }
                for (k, v) in map {
                    println!("{k} = {}", v.as_str().unwrap_or(""));
                }
            }
        }
        ConfigCmd::Get { key } => {
            let settings = client.get("/api/settings").await?;
            match settings.get(&key).and_then(Value::as_str) {
                Some(v) => println!("{v}"),
                None => bail!("no setting '{key}'"),
            }
        }
        ConfigCmd::Set { key, value } => {
            client
                .post("/api/settings", json!({ "key": key, "value": value }))
                .await?;
            println!("set {key}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------

/// Base URL of the server — the same resolution every client uses.
fn server_base() -> String {
    weaver::endpoint::base_url()
}

/// Hit `/health` with a raw GET. Returns `true` only when the server answers
/// `200`; a connection error means "not running" rather than a hard error.
async fn server_is_up(base: &str) -> bool {
    let url = format!("{base}/api/health");
    match reqwest::get(&url).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Render a duration in seconds as a short human-readable string.
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

/// Seconds elapsed since an ISO-8601 `started_at` timestamp.
fn uptime_secs(started_at: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    Some((chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds())
}

/// Poll `/health` until `want` matches the server's liveness, or time out.
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

async fn cmd_server(cmd: ServerCmd) -> Result<()> {
    match cmd {
        ServerCmd::Status => server_status().await,
        ServerCmd::Start => server_start().await,
        ServerCmd::Stop => server_stop().await,
        ServerCmd::Restart => server_restart().await,
    }
}

async fn server_status() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("weaver server: not running");
        return Ok(());
    }
    match weaver::server::read_state() {
        Some(state) => {
            print!("weaver server: running at http://{}  (pid {})", state.addr, state.pid);
            match uptime_secs(&state.started_at) {
                Some(secs) => println!("  up {}", format_uptime(secs)),
                None => println!(),
            }
        }
        None => println!("weaver server: running at {base}  (no state file)"),
    }
    Ok(())
}

async fn server_start() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        println!("weaver server already running at {base}");
        return Ok(());
    }
    spawn_server().await
}

/// Spawn `weaver serve` detached, logging to `<weaver_home>/server.log`, and
/// wait for it to come up.
async fn spawn_server() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("locating the weaver binary")?;
    // Resolve the bind address now and pass it explicitly, so the health check
    // below polls exactly where the new server will listen.
    let addr = weaver::endpoint::bind_addr(None);
    let home = weaver::db::weaver_home();
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating {}", home.display()))?;
    let log_path = home.join("server.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    // Detach into its own process group so it outlives this CLI process.
    let mut command = std::process::Command::new(&exe);
    command
        .arg("serve")
        .arg("--addr")
        .arg(&addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    let child = command.spawn().context("spawning `weaver serve`")?;
    // Deliberately do not wait on the child — it is now independent.
    drop(child);

    let base = format!("http://{addr}");
    if wait_for_health(&base, true, std::time::Duration::from_secs(10)).await {
        println!("weaver server started at {base}");
        Ok(())
    } else {
        bail!(
            "weaver server did not come up within 10s — check the log at {}",
            log_path.display()
        )
    }
}

async fn server_stop() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("no server running");
        return Ok(());
    }
    let state = weaver::server::read_state()
        .ok_or_else(|| anyhow!("server is running but {} is missing or unreadable — \
            stop it manually", weaver::server::state_path().display()))?;

    // Send SIGTERM by shelling out to `kill`, consistent with the tmux/git
    // wrappers — no extra crate needed.
    let status = std::process::Command::new("kill")
        .arg(state.pid.to_string())
        .status()
        .context("failed to run `kill`")?;
    if !status.success() {
        bail!("`kill {}` failed — the process may already be gone", state.pid);
    }

    if wait_for_health(&base, false, std::time::Duration::from_secs(10)).await {
        println!("weaver server stopped (pid {})", state.pid);
        Ok(())
    } else {
        bail!(
            "weaver server (pid {}) did not stop within 10s",
            state.pid
        )
    }
}

async fn server_restart() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        server_stop().await?;
    }
    spawn_server().await
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
    fn uptime_secs_parses_iso_timestamps() {
        // A timestamp far in the past yields a large positive uptime.
        let secs = uptime_secs("2020-01-01T00:00:00.000Z").unwrap();
        assert!(secs > 0);
        // Garbage yields None rather than panicking.
        assert!(uptime_secs("not a timestamp").is_none());
    }

    #[test]
    fn truncate_respects_the_max_length() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long string", 6), "a ver…");
    }
}
