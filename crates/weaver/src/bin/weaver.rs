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

use weaver_core::{branch, config, db, events, issue, note, plan, repo_config};

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
    /// Report the agent's status, or read it back.
    ///
    /// This is the agent's single channel for telling the dashboard how it is
    /// doing. With no arguments it prints the title, goal, status, and
    /// open-issue count. Given a level (`ok`, `attention`, or `blocked`) and an
    /// optional message, it sets both at once: the level drives what the
    /// dashboard surfaces and filters on, and the message is the current-state
    /// note shown beside it. Use `attention` to ask the user to look ("ready
    /// for review", a question) and `blocked` when stuck and needing help; `ok`
    /// covers both progressing normally and being blocked on something external
    /// (a CI run, a PR review) that is not the user.
    SetStatus {
        /// Attention level: `ok`, `attention`, or `blocked`. Omit to read.
        level: Option<String>,
        /// Current-state message, e.g. "Wired up routes; tests pass".
        message: Vec<String>,
    },
    /// Append a note to the current branch.
    Note { text: Vec<String> },
    /// Print a quick orientation for the current branch.
    ///
    /// A one-shot catch-up for an agent picking up (or resuming) a branch: the
    /// goal, the current status, the outstanding tasks (this branch's open
    /// issues and any open sub-trees it delegated), and a line or two of hints
    /// for what to do next. Read-only; derived straight from the database.
    Summary,
    /// Manage the current branch's issue list.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Author and reconcile a structured project plan.
    ///
    /// A plan is a single markdown file (default `docs/plans/<slug>.md`) holding
    /// the design and a task breakdown with stable ids. `weaver plan sync`
    /// reconciles the plan's tasks against the issue ledger. See
    /// `docs/structured-projects.md`.
    Plan {
        #[command(subcommand)]
        cmd: PlanCmd,
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
    /// Show one issue, including the live status of the branch working it.
    Show { id: i64 },
    /// Block until an issue finishes or its sub-tree needs you.
    ///
    /// Polls the issue until it closes (the sub-agent's "done" signal) or — unless
    /// `--closed-only` — until the branch working it raises its attention to
    /// `attention`/`blocked` (it wants you). Prints why it woke. Exits non-zero
    /// if `--timeout` elapses first with the issue still open.
    Wait {
        id: i64,
        /// Give up after this many seconds (0 = wait indefinitely).
        #[arg(long, default_value = "1800")]
        timeout: u64,
        /// Seconds between polls.
        #[arg(long, default_value = "3")]
        interval: u64,
        /// Wake only when the issue closes; ignore the sub-agent's attention.
        #[arg(long)]
        closed_only: bool,
    },
    /// Close an issue.
    Close { id: i64 },
    /// Reopen a closed issue.
    Reopen { id: i64 },
    /// Delete an issue.
    Rm { id: i64 },
}

#[derive(Subcommand)]
enum PlanCmd {
    /// Scaffold a new plan file. The slug defaults to a kebab-case of the title.
    New {
        title: Vec<String>,
        /// Override the slug (the filename stem and link-key prefix).
        #[arg(long)]
        slug: Option<String>,
    },
    /// List the plans on this branch.
    Ls,
    /// Show a plan: its tasks, each with status projected from the issue ledger.
    Show { slug: String },
    /// Reconcile a plan against the issue ledger. Prints the delta; `--apply`
    /// writes it (create / close / update issues, and flag in-flight work).
    Sync {
        slug: String,
        /// Apply the changes instead of just previewing them.
        #[arg(long)]
        apply: bool,
    },
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
        Cmd::SetStatus { level, message } => cmd_set_status(level, message.join(" ")).await,
        Cmd::Note { text } => cmd_note(text.join(" ")).await,
        Cmd::Summary => cmd_summary().await,
        Cmd::Issue { cmd } => cmd_issue(cmd).await,
        Cmd::Plan { cmd } => cmd_plan(cmd).await,
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

/// How many outstanding tasks `weaver summary` lists before collapsing the rest.
const SUMMARY_TASK_CAP: usize = 10;

/// Print a quick orientation for the current branch: the goal, the current
/// status, the outstanding tasks, and a hint or two for what to do next.
///
/// This is the catch-up an agent reads when it picks up a branch. Everything is
/// pulled straight from the database — no LLM, no daemon. It overlaps `set-status`
/// (read), but where that shows an open-issue *count*, summary lists the actual
/// tasks and points at the next action.
async fn cmd_summary() -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;

    // Each section trails the command that drills into it, so the summary
    // doubles as a map of where to look next.
    let goal = if !b.goal.is_empty() {
        b.goal.clone()
    } else if !b.title.is_empty() {
        b.title.clone()
    } else {
        "(none set)".to_string()
    };
    println!("Goal:    {goal}  (weaver goal)");

    let status = if b.description.is_empty() {
        b.attention.clone()
    } else {
        format!("{} — {}", b.attention, b.description)
    };
    println!("Status:  {status}  (weaver set-status)");

    // Plans on this branch — large multi-session efforts. Status comes from the
    // plan file itself, so this stays a cheap read (no issue join).
    let plans = read_plans(&plan_dir(&b));
    match plans.as_slice() {
        [] => println!("Plan:    none  (weaver plan new \"<title>\")"),
        [p] => println!(
            "Plan:    {} [{}]  (weaver plan show {})",
            p.slug, p.status, p.slug
        ),
        many => {
            let slugs = many.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>();
            println!("Plan:    {}  (weaver plan ls)", slugs.join(", "));
        }
    }

    // Outstanding work: this branch's own open issues, then any open sub-trees
    // it delegated (each carrying its sub-agent's live status).
    let open = issue::list_for_branch(&db, &b.repo_root, &b.branch, false).await?;
    let delegated = issue::list_delegated_by(&db, &b.repo_root, &b.branch, false).await?;
    println!();
    if open.is_empty() && delegated.is_empty() {
        println!("Outstanding: none  (weaver issue ls)");
    } else {
        println!(
            "Outstanding ({}):  (weaver issue ls)",
            open.len() + delegated.len()
        );
        for i in open.iter().take(SUMMARY_TASK_CAP) {
            println!("  #{:<4} {}", i.id, i.title);
        }
        if open.len() > SUMMARY_TASK_CAP {
            println!(
                "  (+{} more — weaver issue ls)",
                open.len() - SUMMARY_TASK_CAP
            );
        }
        for i in &delegated {
            let who = working_branch_status(&db, i)
                .await
                .unwrap_or_else(|| i.claimed_branch.clone().unwrap_or_else(|| "?".to_string()));
            println!("  #{:<4} {}  → {who} (delegated)", i.id, i.title);
        }
    }

    // Hints for next steps: the most recent note (where work was left off) plus
    // a generated next-action drawn from the open work.
    println!();
    println!("Next steps:  (weaver log · weaver note)");
    let notes = note::list_for_branch(&db, &b.id).await?;
    if let Some(last) = notes.last() {
        println!("  - last note: {}", truncate(&last.text, 100));
    }
    println!("  - {}", next_action_hint(&open, &delegated));
    Ok(())
}

/// A single suggested next action for `weaver summary`, derived from the open
/// work: pick up the first open task, else poll a delegated sub-tree, else
/// (nothing open) wrap up and open a PR.
fn next_action_hint(open: &[issue::Issue], delegated: &[issue::Issue]) -> String {
    if let Some(first) = open.first() {
        format!(
            "pick up #{} ({}); `weaver issue ls` for the rest",
            first.id,
            truncate(&first.title, 60)
        )
    } else if !delegated.is_empty() {
        format!(
            "{} delegated sub-tree(s) still open — `weaver issue show <id>` to poll",
            delegated.len()
        )
    } else {
        "no open tasks — wrap up and open a PR (`gh pr create`), or `weaver issue add` to track more"
            .to_string()
    }
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

async fn cmd_set_status(level: Option<String>, message: String) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    if let Some(level) = level {
        return cmd_set_status_write(&db, &b, &level, &message).await;
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
    let status = if b.description.is_empty() {
        b.attention.clone()
    } else {
        format!("{} — {}", b.attention, b.description)
    };
    println!("status:      {status}");
    println!("open issues: {open}");
    Ok(())
}

/// Report the agent's status: set the attention level and, when a message is
/// given, the accompanying current-state note (the branch `description`). Writes
/// the branch fields directly (daemon-less) and an `attention` event so a
/// running loom can push the change to the dashboard on its next tick. An empty
/// message leaves the previous message in place — `set-status ok` just lowers
/// the level without wiping what the agent last said.
async fn cmd_set_status_write(
    db: &db::Db,
    b: &branch::Branch,
    level: &str,
    message: &str,
) -> Result<()> {
    let level = level.trim().to_ascii_lowercase();
    if !branch::is_valid_attention(&level) {
        bail!(
            "unknown status '{level}' — expected one of {}",
            branch::ATTENTION_LEVELS.join(", ")
        );
    }
    let message = message.trim();
    branch::set_attention(db, &b.id, &level).await?;
    if !message.is_empty() {
        branch::set_description(db, &b.id, message).await?;
    }
    events::record_local(
        db,
        &b.id,
        "attention",
        json!({ "level": level, "note": message }),
    )
    .await?;
    if message.is_empty() {
        println!("status: {level}");
    } else {
        println!("status: {level} — {message}");
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
                plan_task: None,
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
            // Surface the live status of the branch working this issue — what
            // makes `issue show` a poll of a delegated sub-tree, not just a
            // record lookup.
            if let Some(progress) = working_branch_status(&db, &i).await {
                println!("  working: {progress}");
            }
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
        IssueCmd::Wait {
            id,
            timeout,
            interval,
            closed_only,
        } => {
            cmd_issue_wait(&db, &b.repo_root, id, timeout, interval.max(1), closed_only).await?;
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
    // Sub-trees this branch launched: tracking issues it sourced but another
    // branch is working. Each carries its sub-agent's live status.
    let delegated = issue::list_delegated_by(db, repo_root, target, all).await?;
    if !delegated.is_empty() {
        println!("Delegated by this branch ({}):", delegated.len());
        for i in &delegated {
            let status = working_branch_status(db, i)
                .await
                .unwrap_or_else(|| i.claimed_branch.clone().unwrap_or_else(|| "?".to_string()));
            println!("  {}  → {status}", issue_line(i));
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

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// The directory holding plan files for the current worktree, honoring the
/// per-repo `.weaver/config.toml` `[plan].dir` (default `docs/plans`). Anchored
/// at the worktree (cwd) like `WEAVER.md` resolution, so plans live with the
/// branch the agent is on.
fn plan_dir(b: &branch::Branch) -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let candidates = vec![cwd.clone(), std::path::PathBuf::from(&b.repo_root)];
    cwd.join(repo_config::plan_dir(&candidates))
}

/// Parse the plan at `<dir>/<slug>.md`, or a helpful error if it's missing.
fn load_plan(dir: &std::path::Path, slug: &str) -> Result<plan::Plan> {
    let path = dir.join(format!("{slug}.md"));
    let src = std::fs::read_to_string(&path).map_err(|_| {
        anyhow!(
            "no plan '{slug}' at {} — see `weaver plan ls`",
            path.display()
        )
    })?;
    Ok(plan::parse(slug, &src))
}

/// Every plan file in `dir`, parsed and sorted by slug. Missing dir → empty.
fn read_plans(dir: &std::path::Path) -> Vec<plan::Plan> {
    let mut plans = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return plans;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(src) = std::fs::read_to_string(&path) {
            plans.push(plan::parse(slug, &src));
        }
    }
    plans.sort_by(|a, b| a.slug.cmp(&b.slug));
    plans
}

/// A kebab-case slug from free text: lowercase, runs of non-alphanumerics
/// collapse to a single `-`, trimmed.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

async fn cmd_plan(cmd: PlanCmd) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    let dir = plan_dir(&b);
    match cmd {
        PlanCmd::New { title, slug } => {
            let title = title.join(" ");
            if title.trim().is_empty() {
                bail!("plan title is required");
            }
            let slug = slug.unwrap_or_else(|| slugify(&title));
            if slug.is_empty() {
                bail!("could not derive a slug from the title — pass --slug");
            }
            let path = dir.join(format!("{slug}.md"));
            if path.exists() {
                bail!("plan already exists: {}", path.display());
            }
            // A plan's issues key on `<slug>#<task>` across the whole repo, so a
            // slug already materialized elsewhere would cross-talk with this
            // one. Plans are shared *down a branch lineage* — a sub-session
            // inherits the plan by branching from its parent (the file descends
            // through git), never by re-creating the same slug. So a collision
            // here means an unrelated plan: refuse and let the user rename.
            let claimed = issue::list_for_plan(&db, &b.repo_root, &slug, true).await?;
            if !claimed.is_empty() {
                let owners: std::collections::BTreeSet<&str> = claimed
                    .iter()
                    .filter_map(|i| i.source_branch.as_deref())
                    .collect();
                let who = if owners.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (materialized on {})",
                        owners.into_iter().collect::<Vec<_>>().join(", ")
                    )
                };
                bail!(
                    "plan slug '{slug}' is already in use in this repo{who} — \
                     pick a different title or pass --slug"
                );
            }
            std::fs::create_dir_all(&dir)
                .map_err(|e| anyhow!("creating {}: {e}", dir.display()))?;
            std::fs::write(&path, plan::scaffold(&slug, &title, &b.goal))?;
            events::record_local(&db, &b.id, "plan_created", json!({ "slug": slug })).await?;
            println!("created {}", path.display());
            println!("edit it, then `weaver plan sync {slug} --apply` to materialize its tasks");
        }
        PlanCmd::Ls => {
            let plans = read_plans(&dir);
            if plans.is_empty() {
                println!("(no plans in {})", dir.display());
                return Ok(());
            }
            for p in &plans {
                let tracked = p.tasks.iter().filter(|t| t.materializes()).count();
                println!(
                    "{:<24} {}  ({} tasks, {} tracked) [{}]",
                    p.slug,
                    p.title,
                    p.tasks.len(),
                    tracked,
                    p.status
                );
            }
        }
        PlanCmd::Show { slug } => {
            let p = load_plan(&dir, &slug)?;
            let issues = issue::list_for_plan(&db, &b.repo_root, &p.slug, true).await?;
            println!("{} — {}  [{}]", p.slug, p.title, p.status);
            if p.tasks.is_empty() {
                println!("  (no tasks yet)");
            }
            for t in &p.tasks {
                let key = t.key(&p.slug);
                let issue = issues
                    .iter()
                    .find(|i| i.plan_task.as_deref() == Some(key.as_str()));
                let (marker, detail) = task_status(t, issue);
                println!("  {:<3} [{}] {}", t.id, marker, t.title);
                let mut bits = vec![format!("exec: {}", t.exec)];
                if !t.value.is_empty() {
                    bits.push(format!("value: {}", t.value));
                }
                if !t.deps.is_empty() {
                    bits.push(format!("deps: {}", t.deps.join(", ")));
                }
                bits.push(detail);
                println!("       {}", bits.join("  ·  "));
            }
        }
        PlanCmd::Sync { slug, apply } => {
            let p = load_plan(&dir, &slug)?;
            let issues = issue::list_for_plan(&db, &b.repo_root, &p.slug, true).await?;
            let delta = plan::diff(&p.slug, &p.tasks, &issues);
            if delta.is_empty() {
                println!("plan '{}' is in sync with its issues", p.slug);
                return Ok(());
            }
            print_delta(&delta);
            if apply {
                plan::apply(&db, &b, &p.slug, &delta).await?;
                events::record_local(
                    &db,
                    &b.id,
                    "plan_synced",
                    json!({ "slug": p.slug, "actions": delta.actions.len() }),
                )
                .await?;
                println!("\napplied {} change(s)", delta.actions.len());
                if delta.flags() > 0 {
                    println!(
                        "{} in-flight task(s) flagged — left untouched; status raised to attention",
                        delta.flags()
                    );
                }
            } else {
                println!("\n(dry run — re-run with --apply to write these changes)");
            }
        }
    }
    Ok(())
}

/// A `(marker, detail)` for a plan task given its linked issue (if any), for
/// `weaver plan show`. Status is projected from the ledger, never the file.
fn task_status(t: &plan::PlanTask, issue: Option<&issue::Issue>) -> (char, String) {
    match issue {
        None if t.materializes() => (' ', "not yet materialized".to_string()),
        None => ('·', "untracked".to_string()),
        Some(i) if i.status == "closed" => ('x', format!("done (#{})", i.id)),
        Some(i) => match i.claimed_branch.as_deref() {
            Some(branch) => ('~', format!("in progress ← {branch} (#{})", i.id)),
            None => (' ', format!("backlog (#{})", i.id)),
        },
    }
}

/// Print a reconcile delta as human-readable lines.
fn print_delta(delta: &plan::SyncPlan) {
    use plan::SyncAction::*;
    for action in &delta.actions {
        match action {
            Create { task, title } => println!("  + create issue for {task}: {title}"),
            Close { task, issue_id } => {
                println!("  - close #{issue_id} ({task} removed from plan)")
            }
            UpdateTitle {
                task,
                issue_id,
                title,
            } => println!("  ~ retitle #{issue_id} ({task}) → {title}"),
            Flag {
                task,
                issue_id,
                branch,
                reason,
            } => println!("  ! flag #{issue_id} ({task}, ← {branch}): {reason}"),
        }
    }
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

/// The live status of the branch working `issue`, as `"<branch> · <attention>
/// — <message>"`, or `None` when the issue is unclaimed or its branch row is
/// gone. This is what turns an issue lookup into a poll of a delegated sub-tree.
async fn working_branch_status(db: &db::Db, issue: &issue::Issue) -> Option<String> {
    let claimed = issue.claimed_branch.as_deref()?;
    let row = branch::find_by_repo_branch(db, &issue.repo_root, claimed)
        .await
        .ok()
        .flatten()?;
    let status = if row.description.is_empty() {
        row.attention.clone()
    } else {
        format!("{} — {}", row.attention, row.description)
    };
    Some(format!("{claimed} · {status}"))
}

/// Block until issue `id` finishes (closes) or — unless `closed_only` — its
/// claiming branch raises attention above `ok`. Polls every `interval` seconds;
/// exits the process non-zero if `timeout` (when non-zero) elapses first.
async fn cmd_issue_wait(
    db: &db::Db,
    repo_root: &str,
    id: i64,
    timeout: u64,
    interval: u64,
    closed_only: bool,
) -> Result<()> {
    let issue = ensure_issue_in_repo(db, id, repo_root).await?;
    if issue.status != "open" {
        println!("issue #{id} is {} — nothing to wait for", issue.status);
        return Ok(());
    }
    match working_branch_status(db, &issue).await {
        Some(s) => println!("waiting on #{id} ({}) — {s}", issue.title),
        None => println!("waiting on #{id} ({})", issue.title),
    }

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
        let cur = issue::get(db, id)
            .await?
            .ok_or_else(|| anyhow!("issue #{id} disappeared while waiting"))?;
        if cur.status != "open" {
            println!("issue #{id} closed — sub-tree finished");
            return Ok(());
        }
        if !closed_only {
            if let Some(name) = cur.claimed_branch.as_deref() {
                if let Some(row) = branch::find_by_repo_branch(db, repo_root, name).await? {
                    if row.attention != branch::DEFAULT_ATTENTION {
                        let msg = if row.description.is_empty() {
                            row.attention.clone()
                        } else {
                            format!("{} — {}", row.attention, row.description)
                        };
                        println!("issue #{id} needs you — {name} is {msg}");
                        return Ok(());
                    }
                }
            }
        }
        // Timing out is a real "not done" outcome: report it as an error so the
        // process exits non-zero (callers branch on it) without an ad-hoc
        // `process::exit` that bypasses normal error handling.
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            let progress = working_branch_status(db, &cur)
                .await
                .unwrap_or_else(|| "open".to_string());
            bail!("timed out after {timeout}s — #{id} still open ({progress})");
        }
    }
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
        if event == "session-start" {
            let weaver_md = weaver_md_for_branch(&b);
            print!("{}", weaver_core::agent::session_primer(&weaver_md));
        }
        Ok(())
    })
    .await;
    if let Err(e) = result {
        eprintln!("weaver hook: {e}");
    }
    Ok(())
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
