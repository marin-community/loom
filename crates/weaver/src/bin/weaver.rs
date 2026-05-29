//! weaver — the agent-facing CLI.
//!
//! Every command in this binary talks to the SQLite database directly. The
//! agent can run any of these whether or not the `loom` orchestrator is up.
//! "Current branch" resolves from `$WEAVER_BRANCH` (an internal branch id) or,
//! failing that, from the git checkout containing the current working
//! directory.

use anyhow::{anyhow, bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use weaver_core::{branch, config, db, events, issue, note};

#[derive(Parser)]
#[command(name = "weaver", version, about = "Agent-facing helpers for branches and issues")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print or set the goal of the current branch.
    Goal { text: Vec<String> },
    /// Show the current branch's title, goal, and open-issue count.
    Status,
    /// Set the description of the current branch.
    Describe { text: Vec<String> },
    /// Append a note to the current branch.
    Note { text: Vec<String> },
    /// Manage the current branch's issue list.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Print the resolved repo / branch / branch-id for the current cwd.
    Where,
    /// Print recent events for the current branch.
    Log {
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Record an agent hook event. Writes an `events` row; loom's monitor
    /// consumes it on its next tick.
    Hook {
        /// Hook event name (e.g. `working`, `waiting`, `idle`, `session-start`).
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
enum IssueCmd {
    /// Add a new issue to the current branch.
    Add {
        title: Vec<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        github: Option<i64>,
    },
    /// List issues for the current branch (default: open only).
    Ls {
        #[arg(long)]
        all: bool,
    },
    /// Show one issue.
    Show { id: i64 },
    /// Close an issue.
    Close { id: i64 },
    /// Reopen a closed issue.
    Reopen { id: i64 },
    /// Delete an issue.
    Rm { id: i64 },
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
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
        Cmd::Goal { text } => cmd_goal(text.join(" ")).await,
        Cmd::Status => cmd_status().await,
        Cmd::Describe { text } => cmd_describe(text.join(" ")).await,
        Cmd::Note { text } => cmd_note(text.join(" ")).await,
        Cmd::Issue { cmd } => cmd_issue(cmd).await,
        Cmd::Where => cmd_where().await,
        Cmd::Log { limit } => cmd_log(limit).await,
        Cmd::Hook { event } => cmd_hook(event).await,
        Cmd::Config { cmd } => cmd_config(cmd).await,
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "weaver", &mut std::io::stdout());
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Database-backed commands
// ---------------------------------------------------------------------------

async fn open_db() -> Result<db::Db> {
    db::connect(&db::default_db_path()).await
}

async fn cmd_goal(text: String) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    if text.is_empty() {
        println!("{}", b.goal);
        return Ok(());
    }
    branch::set_goal(&db, &b.id, &text).await?;
    if b.title.is_empty() {
        let title = branch::derive_title(&text);
        branch::set_title(&db, &b.id, &title).await?;
    }
    events::record_local(&db, &b.id, "goal_set", json!({ "goal": text })).await?;
    println!("goal updated");
    Ok(())
}

async fn cmd_describe(text: String) -> Result<()> {
    if text.is_empty() {
        bail!("description text is required");
    }
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    branch::set_description(&db, &b.id, &text).await?;
    events::record_local(&db, &b.id, "note", json!({ "text": "description updated" })).await?;
    println!("description updated");
    Ok(())
}

async fn cmd_note(text: String) -> Result<()> {
    if text.is_empty() {
        bail!("note text is required");
    }
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    note::add(&db, &b.id, &text).await?;
    events::record_local(&db, &b.id, "note", json!({ "text": text })).await?;
    println!("noted");
    Ok(())
}

async fn cmd_where() -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    println!("repo:      {}", b.repo_root);
    println!("branch:    {}", b.branch);
    println!("base:      {}", b.base_branch);
    println!("branch-id: {}", b.id);
    Ok(())
}

async fn cmd_log(limit: i64) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    let history = events::history(&db, &b.id, limit).await?;
    if history.is_empty() {
        println!("(no events)");
        return Ok(());
    }
    for ev in history {
        let detail = if let Some(s) = ev.data.get("text").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(s) = ev.data.get("status").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(s) = ev.data.get("event").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(s) = ev.data.get("goal").and_then(Value::as_str) {
            truncate(s, 60)
        } else {
            ev.data.to_string()
        };
        println!(
            "{}  {:<10}  {}",
            ev.created_at,
            ev.kind,
            truncate(&detail, 100)
        );
    }
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    let open = issue::open_count(&db, &b.id).await.unwrap_or(0);
    println!("repo:        {}", b.repo_root);
    println!("branch:      {}", b.branch);
    println!("base:        {}", b.base_branch);
    if !b.title.is_empty() {
        println!("title:       {}", b.title);
    }
    println!(
        "goal:        {}",
        if b.goal.is_empty() { "(none)" } else { &b.goal }
    );
    if !b.description.is_empty() {
        println!("summary:     {}", b.description);
    }
    println!("open issues: {open}");
    Ok(())
}

async fn cmd_issue(cmd: IssueCmd) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    match cmd {
        IssueCmd::Add { title, body, github } => {
            let title = title.join(" ");
            if title.trim().is_empty() {
                bail!("issue title is required");
            }
            let body = body.unwrap_or_default();
            let i = issue::add(&db, &b.id, &title, &body, github).await?;
            events::record_local(&db, &b.id, "issue_added", json!({ "id": i.id, "title": title }))
                .await?;
            println!("#{} {}", i.id, i.title);
        }
        IssueCmd::Ls { all } => {
            let issues = issue::list_for_branch(&db, &b.id, all).await?;
            if issues.is_empty() {
                println!("(no issues)");
                return Ok(());
            }
            for i in issues {
                let marker = if i.status == "open" { "[ ]" } else { "[x]" };
                let gh = i
                    .github_issue
                    .map(|n| format!(" (gh #{n})"))
                    .unwrap_or_default();
                println!("#{:<4} {} {}{}", i.id, marker, i.title, gh);
            }
        }
        IssueCmd::Show { id } => {
            let i = issue::get(&db, id)
                .await?
                .ok_or_else(|| anyhow!("no issue #{id}"))?;
            if i.branch_id != b.id {
                bail!("issue #{id} belongs to a different branch");
            }
            println!("#{} {}", i.id, i.title);
            println!("  status:  {}", i.status);
            if let Some(n) = i.github_issue {
                println!("  github:  #{n}");
            }
            println!("  created: {}", i.created_at);
            if let Some(c) = &i.closed_at {
                println!("  closed:  {c}");
            }
            if !i.body.is_empty() {
                println!();
                println!("{}", i.body);
            }
        }
        IssueCmd::Close { id } => {
            ensure_issue_on_branch(&db, id, &b.id).await?;
            issue::close(&db, id).await?;
            events::record_local(&db, &b.id, "issue_closed", json!({ "id": id })).await?;
            println!("closed #{id}");
        }
        IssueCmd::Reopen { id } => {
            ensure_issue_on_branch(&db, id, &b.id).await?;
            issue::reopen(&db, id).await?;
            events::record_local(&db, &b.id, "issue_reopened", json!({ "id": id })).await?;
            println!("reopened #{id}");
        }
        IssueCmd::Rm { id } => {
            ensure_issue_on_branch(&db, id, &b.id).await?;
            issue::delete(&db, id).await?;
            println!("removed #{id}");
        }
    }
    Ok(())
}

async fn ensure_issue_on_branch(db: &db::Db, id: i64, branch_id: &str) -> Result<()> {
    let i = issue::get(db, id)
        .await?
        .ok_or_else(|| anyhow!("no issue #{id}"))?;
    if i.branch_id != branch_id {
        bail!("issue #{id} belongs to a different branch");
    }
    Ok(())
}

async fn cmd_hook(event: String) -> Result<()> {
    // Hooks must never break the agent: best-effort, swallow errors.
    let result: Result<()> = (async {
        let db = open_db().await?;
        let b = branch::resolve(&db).await?;
        events::record_local(&db, &b.id, "hook", json!({ "event": event })).await?;
        match event.as_str() {
            "session-start" => {
                print!("{}", weaver_core::agent::session_primer());
            }
            "idle" => {
                // Claude Code's Stop hook pipes a JSON payload on stdin that
                // includes `transcript_path`. Pull the last assistant message
                // out of the transcript and persist it as the branch summary —
                // the agent's own framing of what just happened, no extra
                // headless agent invocation required.
                if let Some(text) = read_last_assistant_text_from_stdin() {
                    branch::set_description(&db, &b.id, &text).await?;
                }
            }
            _ => {}
        }
        Ok(())
    })
    .await;
    if let Err(e) = result {
        eprintln!("weaver hook: {e}");
    }
    Ok(())
}

/// Best-effort: read the Stop-hook stdin payload, locate `transcript_path`,
/// and pull the text of the last assistant message out of the JSONL file.
/// Any failure (no stdin, malformed JSON, missing file) returns `None`.
fn read_last_assistant_text_from_stdin() -> Option<String> {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return None;
    }
    let payload: Value = serde_json::from_str(&buf).ok()?;
    let path = payload.get("transcript_path")?.as_str()?;
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = record.pointer("/message/content")?.as_array()?;
        let mut text = String::new();
        for chunk in content {
            if chunk.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(s) = chunk.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(s);
                }
            }
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        return Some(truncate(text, 600));
    }
    None
}

async fn cmd_config(cmd: ConfigCmd) -> Result<()> {
    let db = open_db().await?;
    match cmd {
        ConfigCmd::List => {
            let settings = config::describe(&db).await?;
            for s in &settings {
                let suffix = if s.is_default { "  (default)" } else { "" };
                println!("{} = {}{suffix}", s.spec.key, s.value);
            }
        }
        ConfigCmd::Get { key } => {
            let settings = config::describe(&db).await?;
            match settings.iter().find(|s| s.spec.key == key) {
                Some(s) => println!("{}", s.value),
                None => bail!("no setting '{key}' — see `weaver config list`"),
            }
        }
        ConfigCmd::Set { key, value } => {
            if config::spec(&key).is_none() {
                bail!("unknown setting '{key}'");
            }
            if let Err(why) = config::validate(&key, &value) {
                bail!("{key}: {why}");
            }
            config::apply(&db, &[(key.clone(), Some(value))]).await?;
            println!("set {key}");
        }
        ConfigCmd::Unset { key } => {
            config::apply(&db, &[(key.clone(), None)]).await?;
            println!("unset {key}");
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_the_max_length() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long string", 6), "a ver…");
    }
}
