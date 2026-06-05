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
#[command(
    name = "weaver",
    version,
    about = "Agent-facing helpers for branches and issues"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print or set the goal of the current branch.
    Goal { text: Vec<String> },
    /// Show the current branch's status, or set the agent's attention level.
    ///
    /// With no arguments it prints the title, goal, attention, and open-issue
    /// count. Given a level (`ok`, `attention`, or `blocked`) and an optional
    /// note, it declares how the agent is doing — what the dashboard surfaces
    /// and filters on. Use `attention` to ask the user to look ("ready for
    /// review", a question) and `blocked` when stuck and needing help; `ok`
    /// covers both progressing normally and being blocked on something external
    /// (a CI run, a PR review) that is not the user.
    Status {
        /// Attention level to set: `ok`, `attention`, or `blocked`. Omit to read.
        level: Option<String>,
        /// Short reason shown beside the level, e.g. "Waiting for PR feedback".
        note: Vec<String>,
    },
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
    /// Add an issue. By default it is claimed by the current branch; `--repo`
    /// creates an unclaimed repo-level backlog item instead.
    Add {
        title: Vec<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        github: Option<i64>,
        /// Create an unclaimed repo backlog item, not attached to this branch.
        #[arg(long)]
        repo: bool,
    },
    /// List issues. Default: this branch's work + the unclaimed repo backlog.
    Ls {
        /// Include closed issues.
        #[arg(long)]
        all: bool,
        /// Show every issue in the repo (all branches + backlog), uncapped.
        #[arg(long)]
        repo: bool,
        /// Show only this branch's claimed issues (suppress the backlog).
        #[arg(long)]
        mine: bool,
        /// Use a different branch as "this branch" (by name).
        #[arg(long)]
        branch: Option<String>,
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
        Cmd::Status { level, note } => cmd_status(level, note.join(" ")).await,
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
        } else if let Some(level) = ev.data.get("level").and_then(Value::as_str) {
            match ev.data.get("note").and_then(Value::as_str) {
                Some(n) if !n.is_empty() => format!("{level} — {n}"),
                _ => level.to_string(),
            }
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

async fn cmd_status(level: Option<String>, note: String) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    if let Some(level) = level {
        return cmd_status_set(&db, &b, &level, &note).await;
    }
    let open = issue::open_count_for_branch(&db, &b.repo_root, &b.branch)
        .await
        .unwrap_or(0);
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
    let attention = if b.attention_note.is_empty() {
        b.attention.clone()
    } else {
        format!("{} — {}", b.attention, b.attention_note)
    };
    println!("attention:   {attention}");
    println!("open issues: {open}");
    Ok(())
}

/// Declare the agent's attention level (and optional note) on the branch. Writes
/// the branch fields directly (daemon-less) and an `attention` event so a
/// running loom can push the change to the dashboard on its next tick.
async fn cmd_status_set(db: &db::Db, b: &branch::Branch, level: &str, note: &str) -> Result<()> {
    let level = level.trim().to_ascii_lowercase();
    if !branch::is_valid_attention(&level) {
        bail!(
            "unknown status '{level}' — expected one of {}",
            branch::ATTENTION_LEVELS.join(", ")
        );
    }
    let note = note.trim();
    branch::set_attention(db, &b.id, &level, note).await?;
    events::record_local(
        db,
        &b.id,
        "attention",
        json!({ "level": level, "note": note }),
    )
    .await?;
    if note.is_empty() {
        println!("status: {level}");
    } else {
        println!("status: {level} — {note}");
    }
    Ok(())
}

/// How many backlog items to print before collapsing the rest into a hint.
const BACKLOG_CAP: usize = 10;

async fn cmd_issue(cmd: IssueCmd) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    match cmd {
        IssueCmd::Add {
            title,
            body,
            github,
            repo,
        } => {
            let title = title.join(" ");
            if title.trim().is_empty() {
                bail!("issue title is required");
            }
            let new = issue::NewIssue {
                repo_root: b.repo_root.clone(),
                github_repo: None,
                source_branch: Some(b.branch.clone()),
                // `--repo` leaves it unclaimed in the backlog; otherwise this
                // branch claims it.
                claimed_branch: (!repo).then(|| b.branch.clone()),
                title: title.clone(),
                body: body.unwrap_or_default(),
                github_issue: github,
            };
            let i = issue::add(&db, &new).await?;
            events::record_local(
                &db,
                &b.id,
                "issue_added",
                json!({ "id": i.id, "title": title }),
            )
            .await?;
            println!("#{} {}", i.id, i.title);
        }
        IssueCmd::Ls {
            all,
            repo,
            mine,
            branch,
        } => {
            let target = branch.unwrap_or_else(|| b.branch.clone());
            if repo {
                cmd_issue_ls_repo(&db, &b.repo_root, &target, all).await?;
            } else {
                cmd_issue_ls_default(&db, &b.repo_root, &target, all, mine).await?;
            }
        }
        IssueCmd::Show { id } => {
            let i = ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            println!("#{} {}", i.id, i.title);
            println!("  status:  {}", i.status);
            println!(
                "  claimed: {}",
                i.claimed_branch.as_deref().unwrap_or("(backlog)")
            );
            if let Some(src) = &i.source_branch {
                println!("  from:    {src}");
            }
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
            ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            issue::close(&db, id).await?;
            events::record_local(&db, &b.id, "issue_closed", json!({ "id": id })).await?;
            println!("closed #{id}");
        }
        IssueCmd::Reopen { id } => {
            ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            issue::reopen(&db, id).await?;
            events::record_local(&db, &b.id, "issue_reopened", json!({ "id": id })).await?;
            println!("reopened #{id}");
        }
        IssueCmd::Rm { id } => {
            ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            issue::delete(&db, id).await?;
            println!("removed #{id}");
        }
    }
    Ok(())
}

fn issue_line(i: &issue::Issue) -> String {
    let marker = if i.status == "open" { "[ ]" } else { "[x]" };
    let gh = i
        .github_issue
        .map(|n| format!(" (gh #{n})"))
        .unwrap_or_default();
    format!("#{:<4} {} {}{}", i.id, marker, i.title, gh)
}

/// Default `ls`: this branch's working set, plus the unclaimed repo backlog
/// (capped). `--mine` drops the backlog section.
async fn cmd_issue_ls_default(
    db: &db::Db,
    repo_root: &str,
    target: &str,
    all: bool,
    mine: bool,
) -> Result<()> {
    let working = issue::list_for_branch(db, repo_root, target, all).await?;
    let mut printed = false;
    if !working.is_empty() {
        println!("On this branch ({}):", working.len());
        for i in &working {
            println!("  {}", issue_line(i));
        }
        printed = true;
    }
    if !mine {
        let backlog = issue::list_backlog(db, repo_root, all).await?;
        if !backlog.is_empty() {
            let shown = backlog.len().min(BACKLOG_CAP);
            println!(
                "Repo backlog ({} unclaimed, showing {}):",
                backlog.len(),
                shown
            );
            for i in backlog.iter().take(BACKLOG_CAP) {
                println!("  {}", issue_line(i));
            }
            if backlog.len() > BACKLOG_CAP {
                println!(
                    "  (+{} more — weaver issue ls --repo)",
                    backlog.len() - BACKLOG_CAP
                );
            }
            printed = true;
        }
    }
    if !printed {
        println!("(no issues)");
    }
    Ok(())
}

/// `ls --repo`: every open (or, with `--all`, every) issue in the repo, grouped
/// into this branch / unclaimed backlog / other branches.
async fn cmd_issue_ls_repo(db: &db::Db, repo_root: &str, target: &str, all: bool) -> Result<()> {
    let issues = issue::list_for_repo(db, repo_root, all).await?;
    if issues.is_empty() {
        println!("(no issues)");
        return Ok(());
    }
    let mut mine = Vec::new();
    let mut backlog = Vec::new();
    let mut others = Vec::new();
    for i in &issues {
        match i.claimed_branch.as_deref() {
            Some(b) if b == target => mine.push(i),
            Some(_) => others.push(i),
            None => backlog.push(i),
        }
    }
    let section = |title: String, items: &[&issue::Issue]| {
        if items.is_empty() {
            return;
        }
        println!("{title}");
        for i in items {
            // Annotate cross-branch items with who holds them.
            let who = i
                .claimed_branch
                .as_deref()
                .filter(|b| *b != target)
                .map(|b| format!("  ← {b}"))
                .unwrap_or_default();
            println!("  {}{}", issue_line(i), who);
        }
    };
    section(format!("On this branch ({}):", mine.len()), &mine);
    section(
        format!("Repo backlog ({} unclaimed):", backlog.len()),
        &backlog,
    );
    section(format!("Other branches ({}):", others.len()), &others);
    Ok(())
}

/// Confirm an issue exists and lives in `repo_root`. Cross-*repo* access is the
/// real mistake to guard; within a repo, claimed and backlog items are all fair
/// game. Returns the issue so callers can reuse it.
async fn ensure_issue_in_repo(db: &db::Db, id: i64, repo_root: &str) -> Result<issue::Issue> {
    let i = issue::get(db, id)
        .await?
        .ok_or_else(|| anyhow!("no issue #{id}"))?;
    if i.repo_root != repo_root {
        bail!("issue #{id} belongs to a different repo");
    }
    Ok(i)
}

/// The WEAVER.md to inject at session start: the repo's own copy when it ships
/// one, else the builtin. We look in the worktree the hook is actually running
/// in (its cwd at launch is the worktree root) and then in the primary checkout,
/// so a `WEAVER.md` committed on the base branch is picked up either way.
fn weaver_md_for_branch(branch: &branch::Branch) -> String {
    let candidates = std::env::current_dir()
        .ok()
        .into_iter()
        .chain(std::iter::once(std::path::PathBuf::from(&branch.repo_root)));
    for dir in candidates {
        if let Ok(md) = std::fs::read_to_string(dir.join("WEAVER.md")) {
            if !md.trim().is_empty() {
                return md;
            }
        }
    }
    weaver_core::agent::builtin_weaver_md().to_string()
}

async fn cmd_hook(event: String) -> Result<()> {
    // Hooks must never break the agent: best-effort, swallow errors.
    let result: Result<()> = (async {
        let db = open_db().await?;
        let b = branch::resolve(&db).await?;
        events::record_local(&db, &b.id, "hook", json!({ "event": event })).await?;
        match event.as_str() {
            "session-start" => {
                let weaver_md = weaver_md_for_branch(&b);
                print!("{}", weaver_core::agent::session_primer(&weaver_md));
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
