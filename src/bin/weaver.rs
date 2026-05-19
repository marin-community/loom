//! weaver CLI — a thin client over the local weaver server, plus `serve`.

use anyhow::{anyhow, bail, Result};
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
        #[arg(long, default_value = "127.0.0.1:7878")]
        addr: String,
    },
    /// Create a new workspace: worktree + tmux session + agent.
    New {
        /// High-level goal for the workspace.
        goal: Vec<String>,
        /// Base branch to fork from (defaults to the repo's current branch).
        #[arg(long)]
        base: Option<String>,
        /// Agent to launch: claude (default), shell, or a custom command.
        #[arg(long)]
        agent: Option<String>,
        /// Explicit workspace name / branch slug.
        #[arg(long)]
        name: Option<String>,
        /// GitHub issue number to link and seed the goal from.
        #[arg(long)]
        issue: Option<i64>,
    },
    /// List all workspaces.
    Ls,
    /// Show one workspace in detail (or all when no id given).
    Status { id: Option<String> },
    /// Attach your terminal to a workspace's tmux session.
    Attach { id: String },
    /// Send a line of text to a workspace's agent.
    Send { id: String, text: Vec<String> },
    /// Force a fresh summary of a workspace now.
    Summary { id: String },
    /// Merge a workspace's branch into its base branch.
    Merge { id: String },
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
    /// Generate shell completions.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
    List,
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
            weaver::server::run(&addr).await
        }
        Cmd::New {
            goal,
            base,
            agent,
            name,
            issue,
        } => cmd_new(goal.join(" "), base, agent, name, issue).await,
        Cmd::Ls => cmd_ls().await,
        Cmd::Status { id } => cmd_status(id).await,
        Cmd::Attach { id } => cmd_attach(id).await,
        Cmd::Send { id, text } => cmd_send(id, text.join(" ")).await,
        Cmd::Summary { id } => cmd_summary(id).await,
        Cmd::Merge { id } => cmd_merge(id).await,
        Cmd::Rm { id, keep_branch } => cmd_rm(id, keep_branch).await,
        Cmd::Open => cmd_open().await,
        Cmd::Goal => cmd_goal().await,
        Cmd::Description { text } => cmd_description(text.join(" ")).await,
        Cmd::Note { text } => cmd_note(text.join(" ")).await,
        Cmd::Hook { workspace, event } => cmd_hook(workspace, event).await,
        Cmd::Config { cmd } => cmd_config(cmd).await,
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
    println!("  goal:   {}", str_field(&ws, "goal"));
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
    println!("{:<10}  {:<9}  {:<22}  GOAL", "ID", "STATUS", "NAME");
    for ws in rows {
        println!(
            "{:<10}  {:<9}  {:<22}  {}",
            str_field(&ws, "id"),
            str_field(&ws, "status"),
            truncate(str_field(&ws, "name"), 22),
            truncate(str_field(&ws, "goal"), 48),
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
    println!("  status:   {}", str_field(&ws, "status"));
    println!("  goal:     {}", str_field(&ws, "goal"));
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
    let client = Client::new();
    let _ = client
        .post("/api/hook", json!({ "workspace": workspace, "event": event }))
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
