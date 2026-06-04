//! loom — the orchestration CLI.
//!
//! Most subcommands talk to the running loom daemon over HTTP (session
//! lifecycle, summary, merge, adopt). `serve` runs the daemon itself;
//! `start`/`stop`/`restart`/`status` manage its background lifecycle. To
//! interact with an agent, `attach` to its tmux (the browser terminal is the
//! other interaction surface).

use anyhow::{anyhow, bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use loom::client::Client;

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
    /// Run the loom server (REST API + Vue UI + monitor + summary loops).
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

    /// Launch a new session: worktree + tmux + agent.
    Launch {
        /// Optional name (becomes the `weaver/<name>` branch slug). When the
        /// name matches an existing branch in the repo, that branch is adopted
        /// instead of creating a new one.
        name: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        issue: Option<i64>,
        /// Claim an existing weaver issue (by id) for this session: seeds the
        /// goal from it and moves it out of the repo backlog.
        #[arg(long)]
        claim: Option<i64>,
        /// Attach to this existing branch rather than creating a new one.
        #[arg(long)]
        branch: Option<String>,
        /// Model tier: haiku, sonnet, or opus. Omit to inherit the configured
        /// `agent.claude_args`.
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort: low, medium, high, xhigh, or max. Omit to inherit
        /// the configured `agent.claude_args`.
        #[arg(long)]
        effort: Option<String>,
    },
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
    /// Force a fresh summary of a session.
    Summary { branch: String },
    /// Merge a session's branch into its base branch.
    Merge { branch: String },
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
        Cmd::Launch {
            name,
            agent,
            base,
            goal,
            title,
            issue,
            claim,
            branch,
            model,
            effort,
        } => cmd_launch(name, agent, base, goal, title, issue, claim, branch, model, effort).await,
        Cmd::Ps => cmd_ps().await,
        Cmd::Issues { all, backlog } => cmd_issues(all, backlog).await,
        Cmd::Show { branch } => cmd_show(branch).await,
        Cmd::Attach { branch } => cmd_attach(branch).await,
        Cmd::Summary { branch } => cmd_summary(branch).await,
        Cmd::Merge { branch } => cmd_merge(branch).await,
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

#[allow(clippy::too_many_arguments)]
async fn cmd_launch(
    name: Option<String>,
    agent: Option<String>,
    base: Option<String>,
    goal: Option<String>,
    title: Option<String>,
    issue: Option<i64>,
    claim: Option<i64>,
    branch: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    let client = Client::new();
    let cwd = std::env::current_dir()?;
    let goal = goal.unwrap_or_default();
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
    println!("  attach: loom attach {id}");
    Ok(())
}

async fn cmd_ps() -> Result<()> {
    let client = Client::new();
    let list = client.get("/api/sessions").await?;
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no sessions — start one with `loom launch \"<name>\"`");
        return Ok(());
    }
    println!("{:<10}  {:<9}  {:<24}  TITLE", "ID", "STATUS", "NAME");
    for ws in rows {
        println!(
            "{:<10}  {:<9}  {:<24}  {}",
            str_field(&ws, "id"),
            str_field(&ws, "status"),
            truncate(branch_str(&ws, "name"), 24),
            truncate(branch_str(&ws, "title"), 46),
        );
    }
    Ok(())
}

async fn cmd_issues(all: bool, backlog: bool) -> Result<()> {
    let client = Client::new();
    let cwd = std::env::current_dir()?;
    let scope = if backlog { "backlog" } else { "repo" };
    let path = format!(
        "/api/repos/issues?cwd={}&all={all}&scope={scope}",
        encode_query(&cwd.display().to_string()),
    );
    let rows = client.get(&path).await?.as_array().cloned().unwrap_or_default();
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
    let client = Client::new();
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
    let goal = branch_str(ws, "goal");
    println!(
        "  goal:     {}",
        if goal.is_empty() { "(none)" } else { goal }
    );
    let description = branch_str(ws, "description");
    if !description.is_empty() {
        println!("  summary:  {description}");
    }
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
    println!("  activity: {}", str_field(ws, "last_activity_at"));
    let prompt = str_field(ws, "pending_prompt");
    if !prompt.is_empty() {
        println!("  waiting on:");
        for line in prompt.lines() {
            println!("    {line}");
        }
    }
}

async fn cmd_attach(key: String) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let client = Client::new();
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

async fn cmd_summary(key: String) -> Result<()> {
    let client = Client::new();
    println!("summarizing… (this runs a headless agent and may take a moment)");
    let res = client
        .post(&format!("/api/sessions/{key}/summarize"), json!({}))
        .await?;
    println!("{}", str_field(&res, "description"));
    Ok(())
}

async fn cmd_merge(key: String) -> Result<()> {
    let client = Client::new();
    let res = client
        .post(&format!("/api/sessions/{key}/merge"), json!({}))
        .await?;
    println!("merged {}", str_field(&res, "branch"));
    let output = str_field(&res, "output");
    if !output.is_empty() {
        println!("{output}");
    }
    Ok(())
}

async fn cmd_adopt(key: String) -> Result<()> {
    let client = Client::new();
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
    let client = Client::new();
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
}
