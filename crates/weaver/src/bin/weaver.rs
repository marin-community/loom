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

use weaver_core::{artifact, branch, config, db, events, issue, tags};

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
    /// Print the goal, or set it (`goal set`).
    ///
    /// With no subcommand, prints the current branch's goal. `goal set` writes a
    /// new goal — from text args, a `--file`, or stdin (`-`). A long markdown
    /// goal stops being a shell-quoting exercise, so an agent can maintain the
    /// goal as understanding evolves. The Overview renders it through the same
    /// markdown pipeline as artifacts, projection included.
    Goal {
        #[command(subcommand)]
        cmd: Option<GoalCmd>,
    },
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
    /// Read, set, or clear a tag on a session.
    ///
    /// A tag is a single-valued `(key, value)` annotation on a branch with a
    /// one-line note and an author. The well-known loud keys are `attention`
    /// (the agent's own signal, normally written by `set-status`) and `triage`
    /// (an overlooker's outside assessment); both accept `attention` or
    /// `blocked`. Any other key is free-form and quiet. Daemon-less, like
    /// `set-status`.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Print a quick orientation for the current branch.
    ///
    /// A one-shot catch-up for an agent picking up (or resuming) a branch: the
    /// goal, the current status, the outstanding tasks (this branch's open
    /// issues and any open sub-trees it delegated), and a line or two of hints
    /// for what to do next. Read-only; derived straight from the database.
    Summary,
    /// Print the full weaver workflow guide (the WEAVER.md for this branch).
    ///
    /// The same primer injected at session start — how a weaver session works
    /// and what is expected of the agent. Re-read it when you need the full
    /// rules back (e.g. after a context compaction, when only the concise
    /// catch-up was replayed). Uses the repo's own `WEAVER.md` when it ships
    /// one, else the builtin.
    Readme,
    /// Manage the current branch's issue list.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Read and write artifacts — named, versioned documents stored in weaver.
    ///
    /// An artifact is a design, report, diagram, or plan the agent writes *to
    /// weaver*, not the repo: durable, out-of-repo, and surviving archive. Every
    /// write appends an immutable revision. Scoped to the current branch by
    /// default; `--repo` publishes it repo-shared (one plan, many child
    /// sessions). See `docs/artifacts.md`.
    Artifact {
        #[command(subcommand)]
        cmd: ArtifactCmd,
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
    /// Label an issue: set (insert or replace) a free-form `(key, value)` tag.
    ///
    /// Issue tags are quiet annotations (priority, area, kind, …) rendered as
    /// pills in the loom Issues pane — there is no loud `attention`/`triage`
    /// ladder. The value must be non-empty; clear a label with `issue untag`.
    Tag {
        id: i64,
        /// The tag key, e.g. `priority` or `area`.
        key: String,
        /// The value to store, e.g. `high`.
        value: String,
        /// One-line reason accompanying the tag.
        #[arg(long, default_value = "")]
        note: String,
        /// Who is setting it (attribution); defaults to `agent`.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Clear an issue label — delete the `(key)` tag.
    Untag { id: i64, key: String },
}

#[derive(Subcommand)]
enum GoalCmd {
    /// Set the goal from text args, a `--file`, or stdin (`-`).
    Set {
        /// Goal text. Joined with spaces. Omit when using `--file` or `-`.
        text: Vec<String>,
        /// Read the goal from a file instead of the text args.
        #[arg(long)]
        file: Option<String>,
    },
}

#[derive(Subcommand)]
enum ArtifactCmd {
    /// Write an artifact: append a new revision (creating it if absent). Reads
    /// `<file>`, or stdin when `<file>` is `-` or omitted.
    Write {
        /// The artifact name (its identity within the scope), e.g. `plan`.
        name: String,
        /// File to read the content from; `-` or omitted reads stdin.
        file: Option<String>,
        /// A human title for the artifact (envelope metadata).
        #[arg(long, default_value = "")]
        title: String,
        /// The content kind; defaults to `markdown`.
        #[arg(long, default_value = "markdown")]
        kind: String,
        /// Publish repo-shared (visible to every branch) instead of scoping it
        /// to the current branch.
        #[arg(long)]
        repo: bool,
    },
    /// List artifacts: this branch's plus the repo-shared ones. `--repo` lists
    /// every artifact in the repo, all scopes.
    Ls {
        /// List every artifact in the repo, regardless of scope.
        #[arg(long)]
        repo: bool,
    },
    /// Show an artifact's content (latest revision by default). `--meta` prints
    /// the envelope (id, name, kind, title, scope, latest rev, timestamps).
    Show {
        name: String,
        /// Show a specific revision instead of the latest.
        #[arg(long)]
        rev: Option<i64>,
        /// Print the envelope metadata instead of the content.
        #[arg(long)]
        meta: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
    List,
}

#[derive(Subcommand)]
enum TagCmd {
    /// Set (insert or replace) a tag. The loud keys (`attention`, `triage`)
    /// accept only `attention` or `blocked`; clear them with `tag rm`. Any
    /// other key is free-form. Defaults to the current branch; `--session`
    /// targets another.
    Set {
        /// The tag key, e.g. `attention`, `triage`, or any free-form name.
        key: String,
        /// The value to store.
        value: String,
        /// One-line reason accompanying the tag.
        #[arg(long, default_value = "")]
        note: String,
        /// The session to tag: an id, `repo:branch`, or unambiguous prefix.
        /// Defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
        /// Who is setting it (attribution); defaults to `manual`.
        #[arg(long, default_value = "manual")]
        by: String,
    },
    /// Clear a tag — return that axis to its calm/default (absent) state.
    Rm {
        /// The tag key to clear.
        key: String,
        /// The session to clear it on; defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
    },
    /// List every tag on a session (defaults to the current branch).
    Ls {
        /// The session to list; defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
    },
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
        Cmd::Goal { cmd } => cmd_goal(cmd).await,
        Cmd::SetStatus { level, message } => cmd_set_status(level, message.join(" ")).await,
        Cmd::Tag { cmd } => cmd_tag(cmd).await,
        Cmd::Summary => cmd_summary().await,
        Cmd::Readme => cmd_readme().await,
        Cmd::Issue { cmd } => cmd_issue(cmd).await,
        Cmd::Artifact { cmd } => cmd_artifact(cmd).await,
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

/// Read content from a file path, or stdin when `path` is `None` or `"-"`.
fn read_file_or_stdin(path: Option<&str>) -> Result<String> {
    use std::io::Read;
    match path {
        Some(p) if p != "-" => std::fs::read_to_string(p).map_err(|e| anyhow!("reading {p}: {e}")),
        _ => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow!("reading stdin: {e}"))?;
            Ok(buf)
        }
    }
}

/// Print the goal, or set it. With no subcommand prints the current goal;
/// `goal set` writes a new one from text args, a `--file`, or stdin (`-`).
async fn cmd_goal(cmd: Option<GoalCmd>) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    let Some(GoalCmd::Set { text, file }) = cmd else {
        println!("{}", b.goal);
        return Ok(());
    };
    // A `--file` (or `-` text) reads from disk/stdin; otherwise join the args.
    let goal = if let Some(path) = file {
        read_file_or_stdin(Some(&path))?
    } else if text.as_slice() == ["-"] {
        read_file_or_stdin(None)?
    } else {
        text.join(" ")
    };
    let goal = goal.trim_end().to_string();
    if goal.is_empty() {
        bail!("a goal is required — pass text, --file <path>, or pipe via '-'");
    }
    branch::set_goal(&db, &b.id, &goal).await?;
    if b.title.is_empty() {
        let title = branch::derive_title(&goal);
        branch::set_title(&db, &b.id, &title).await?;
    }
    events::record_local(&db, &b.id, "goal_set", json!({ "goal": goal })).await?;
    println!("goal updated");
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
    print!("{}", render_summary(&db, &b).await?);
    Ok(())
}

/// Render the `weaver summary` catch-up as a string (see [`cmd_summary`]). Kept
/// separate from the printing so the post-compaction hook can replay the same
/// text into the agent's context as `additionalContext` (see [`cmd_hook`]).
async fn render_summary(db: &db::Db, b: &branch::Branch) -> Result<String> {
    use std::fmt::Write as _;
    let mut out = String::new();

    // Each section trails the command that drills into it, so the summary
    // doubles as a map of where to look next.
    let goal = if !b.goal.is_empty() {
        b.goal.clone()
    } else if !b.title.is_empty() {
        b.title.clone()
    } else {
        "(none set)".to_string()
    };
    let _ = writeln!(out, "Goal:    {goal}  (weaver goal)");

    let attention = resolve_attention(db, &b.id).await?;
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    let _ = writeln!(out, "Status:  {status}  (weaver set-status)");

    // Artifacts visible from this branch (its own + repo-shared) — the documents
    // the agent has written to weaver (designs, reports, the `plan`).
    let artifacts = artifact::list_for_session(db, &b.repo_root, &b.id)
        .await
        .unwrap_or_default();
    match artifacts.as_slice() {
        [] => {
            let _ = writeln!(
                out,
                "Artifacts: none  (weaver artifact write <name> <file>)"
            );
        }
        [a] => {
            let _ = writeln!(
                out,
                "Artifacts: {} [rev {}]  (weaver artifact show {})",
                a.name, a.rev, a.name
            );
        }
        many => {
            let names = many.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
            let _ = writeln!(out, "Artifacts: {}  (weaver artifact ls)", names.join(", "));
        }
    }

    // Outstanding work: this branch's own open issues, then any open sub-trees
    // it delegated (each carrying its sub-agent's live status).
    let open = issue::list_for_branch(db, &b.repo_root, &b.branch, false).await?;
    let delegated = issue::list_delegated_by(db, &b.repo_root, &b.branch, false).await?;
    out.push('\n');
    if open.is_empty() && delegated.is_empty() {
        let _ = writeln!(out, "Outstanding: none  (weaver issue ls)");
    } else {
        let total = open.len() + delegated.len();
        let _ = writeln!(out, "Outstanding ({total}):  (weaver issue ls)");
        // Cap the whole list (own issues first, then delegated sub-trees) so a
        // branch that delegated many sub-trees can't blow the summary up; the
        // overflow collapses into one trailing line.
        let mut shown = 0;
        for i in open.iter().take(SUMMARY_TASK_CAP) {
            let _ = writeln!(out, "  #{:<4} {}", i.id, i.title);
            shown += 1;
        }
        for i in delegated.iter().take(SUMMARY_TASK_CAP - shown) {
            let who = working_branch_status(db, i)
                .await
                .unwrap_or_else(|| i.claimed_branch.clone().unwrap_or_else(|| "?".to_string()));
            let _ = writeln!(out, "  #{:<4} {}  → {who} (delegated)", i.id, i.title);
            shown += 1;
        }
        if total > shown {
            let _ = writeln!(out, "  (+{} more — weaver issue ls)", total - shown);
        }
    }

    // Hint for the next step: a generated next-action drawn from the open work.
    // The current status (where work was left off) is already on the `Status:`
    // line above, sourced from the status-description trail.
    out.push('\n');
    let _ = writeln!(out, "Next steps:  (weaver log · weaver set-status)");
    let _ = writeln!(out, "  - {}", next_action_hint(&open, &delegated));
    Ok(out)
}

/// Print the full weaver workflow guide for this branch (the repo's own
/// `WEAVER.md` when it ships one, else the builtin). The same primer injected at
/// session start; `weaver readme` lets the agent pull it back on demand — most
/// usefully after a context compaction, when only the concise catch-up was
/// replayed.
async fn cmd_readme() -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    print!("{}", weaver_md_for_branch(&b));
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
    let attention = resolve_attention(&db, &b.id).await?;
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    println!("status:      {status}");
    println!("open issues: {open}");
    Ok(())
}

/// The attention value that means "calm" — the default when a branch has no
/// `attention` tag. Never stored (absence is calm); it is both the resolved
/// value the status reads fall back to and the `set-status` input that clears
/// the tag.
const CALM: &str = "ok";

/// The resolved attention level for a branch: the `attention` tag's value, or
/// [`CALM`] when there is no tag. The single read path the CLI's status displays
/// and `issue wait` gating share. A DB read error propagates rather than masking
/// as calm.
async fn resolve_attention(db: &db::Db, branch_id: &str) -> Result<String> {
    Ok(tags::get(db, branch_id, tags::ATTENTION_KEY)
        .await?
        .map(|t| t.value)
        .unwrap_or_else(|| CALM.to_string()))
}

/// Report the agent's status: set the attention level and, when a message is
/// given, the accompanying current-state note (the branch `description`). The
/// level lives on the `attention` tag — `ok` clears it (absence is the calm
/// state), `attention`/`blocked` set it. Writes the description directly
/// (daemon-less) and records a `tag` event so a running loom can push the change
/// to the dashboard on its next tick. An empty message leaves the previous
/// message in place — `set-status ok` just lowers the level without wiping what
/// the agent last said.
async fn cmd_set_status_write(
    db: &db::Db,
    b: &branch::Branch,
    level: &str,
    message: &str,
) -> Result<()> {
    let level = level.trim().to_ascii_lowercase();
    // `ok` is a valid *input* (return to calm) but is never stored — it clears
    // the tag. The two storable levels come from the tags registry.
    if level != CALM && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        bail!("unknown status '{level}' — expected one of ok, attention, blocked");
    }
    let message = message.trim();
    if !message.is_empty() {
        branch::set_description(db, &b.id, message).await?;
    }
    // `ok` returns to calm by clearing the tag; the two loud levels upsert it.
    // Either way record a `tag` event (empty value = cleared) so the monitor
    // re-broadcasts and live dashboards refresh.
    let value = if level == CALM {
        tags::clear(db, &b.id, tags::ATTENTION_KEY).await?;
        ""
    } else {
        tags::set(db, &b.id, tags::ATTENTION_KEY, &level, "", "agent").await?;
        level.as_str()
    };
    events::record_local(
        db,
        &b.id,
        "tag",
        json!({ "key": tags::ATTENTION_KEY, "value": value, "note": "", "by": "agent" }),
    )
    .await?;
    if message.is_empty() {
        println!("status: {level}");
    } else {
        println!("status: {level} — {message}");
    }
    Ok(())
}

/// Resolve the branch a tag command targets: the named `--session` (an id,
/// `repo:branch`, or unambiguous prefix) when given, else the current branch.
async fn resolve_tag_target(db: &db::Db, session: Option<&str>) -> Result<branch::Branch> {
    match session {
        Some(key) => branch::resolve_key(db, key)
            .await?
            .ok_or_else(|| anyhow!("no session matching '{key}'")),
        None => branch::resolve(db).await,
    }
}

/// Set, clear, or list a tag on a branch. Tags unify the agent's `attention`
/// self-report and an overlooker's `triage` assessment with any free-form axis.
/// Writes the tag directly (daemon-less) and records a `tag` event so a running
/// loom can push the change to the dashboard on its next tick.
async fn cmd_tag(cmd: TagCmd) -> Result<()> {
    let db = open_db().await?;
    match cmd {
        TagCmd::Set {
            key,
            value,
            note,
            session,
            by,
        } => {
            let b = resolve_tag_target(&db, session.as_deref()).await?;
            let key = key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if !tags::is_valid_value(key, value) {
                if tags::is_loud(key) {
                    bail!(
                        "'{key}' accepts only {} — use `weaver tag rm {key}` to clear it",
                        tags::ATTENTION_VALUES.join(", ")
                    );
                }
                bail!("a tag value cannot be empty — use `weaver tag rm {key}` to clear it");
            }
            tags::set(&db, &b.id, key, value, note, by).await?;
            events::record_local(
                &db,
                &b.id,
                "tag",
                json!({ "key": key, "value": value, "note": note, "by": by }),
            )
            .await?;
            if note.is_empty() {
                println!("tag: {} → {key} = {value} (by {by})", b.branch);
            } else {
                println!("tag: {} → {key} = {value} (by {by}) — {note}", b.branch);
            }
        }
        TagCmd::Rm { key, session } => {
            let b = resolve_tag_target(&db, session.as_deref()).await?;
            let key = key.trim();
            tags::clear(&db, &b.id, key).await?;
            events::record_local(
                &db,
                &b.id,
                "tag",
                json!({ "key": key, "value": "", "note": "", "by": "manual" }),
            )
            .await?;
            println!("tag: {} → cleared {key}", b.branch);
        }
        TagCmd::Ls { session } => {
            let b = resolve_tag_target(&db, session.as_deref()).await?;
            let all = tags::list(&db, &b.id).await?;
            if all.is_empty() {
                println!("(no tags)");
                return Ok(());
            }
            for t in &all {
                let by = if t.set_by.is_empty() {
                    String::new()
                } else {
                    format!("  (by {})", t.set_by)
                };
                let note = if t.note.is_empty() {
                    String::new()
                } else {
                    format!("  — {}", t.note)
                };
                println!("{} = {}{by}{note}", t.key, t.value);
            }
        }
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
            let tags = issue::list_tags(&db, i.id).await?;
            if !tags.is_empty() {
                let rendered = tags
                    .iter()
                    .map(|t| format!("{}={}", t.key, t.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  tags:    {rendered}");
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
        IssueCmd::Tag {
            id,
            key,
            value,
            note,
            by,
        } => {
            ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            let key = key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if key.is_empty() {
                bail!("a tag key is required");
            }
            if value.is_empty() {
                bail!(
                    "a tag value cannot be empty — use `weaver issue untag {id} {key}` to clear it"
                );
            }
            issue::set_tag(&db, id, key, value, note, by).await?;
            if note.is_empty() {
                println!("tag: #{id} → {key} = {value} (by {by})");
            } else {
                println!("tag: #{id} → {key} = {value} (by {by}) — {note}");
            }
        }
        IssueCmd::Untag { id, key } => {
            ensure_issue_in_repo(&db, id, &b.repo_root).await?;
            issue::clear_tag(&db, id, key.trim()).await?;
            println!("tag: #{id} → cleared {}", key.trim());
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
// Artifacts
// ---------------------------------------------------------------------------

/// Read, write, and list artifacts — named, versioned documents stored in
/// weaver.db. Scoped to the current branch by default; `--repo` is repo-shared.
async fn cmd_artifact(cmd: ArtifactCmd) -> Result<()> {
    let db = open_db().await?;
    let b = branch::resolve(&db).await?;
    match cmd {
        ArtifactCmd::Write {
            name,
            file,
            title,
            kind,
            repo,
        } => {
            let content = read_file_or_stdin(file.as_deref())?;
            // `--repo` writes the repo-shared scope (branch_id = NULL); otherwise
            // the artifact is scoped to this branch.
            let branch_id = (!repo).then_some(b.id.as_str());
            let a = artifact::write(
                &db,
                &artifact::NewRevision {
                    repo_root: &b.repo_root,
                    branch_id,
                    name: name.trim(),
                    kind: kind.trim(),
                    title: title.trim(),
                    content: &content,
                    author: "agent",
                },
            )
            .await?;
            events::record_local(
                &db,
                &b.id,
                "artifact_written",
                json!({ "name": a.name, "rev": a.rev, "title": a.title }),
            )
            .await?;
            // Print the dashboard URL so the agent can hand it to the user. The
            // write already succeeded (a plain DB write); without a running loom
            // we fall back to the name + scope.
            let scope = if repo { "repo-shared" } else { "this branch" };
            match db::dashboard_addr() {
                Some(addr) => println!(
                    "http://{addr}/s/{}/artifacts/{}  (rev {}, {scope})",
                    b.id, a.name, a.rev
                ),
                None => println!("{} (rev {}, {scope})", a.name, a.rev),
            }
        }
        ArtifactCmd::Ls { repo } => {
            let artifacts = if repo {
                artifact::list_for_repo(&db, &b.repo_root).await?
            } else {
                artifact::list_for_session(&db, &b.repo_root, &b.id).await?
            };
            if artifacts.is_empty() {
                println!("(no artifacts)");
                return Ok(());
            }
            for a in &artifacts {
                // A branch-scoped artifact is prefixed by its owning branch id;
                // a repo-shared one is marked so the scope is legible at a glance.
                let scope = match &a.branch_id {
                    Some(bid) => format!("{bid}/"),
                    None => "repo:".to_string(),
                };
                let title = if a.title.is_empty() {
                    String::new()
                } else {
                    format!("  {}", a.title)
                };
                println!("{scope}{:<24} [rev {}] {}{title}", a.name, a.rev, a.kind);
            }
        }
        ArtifactCmd::Show { name, rev, meta } => {
            let a = artifact::get(&db, &b.repo_root, &b.id, name.trim())
                .await?
                .ok_or_else(|| {
                    anyhow!("no artifact '{}' — see `weaver artifact ls`", name.trim())
                })?;
            if meta {
                println!("id:      {}", a.id);
                println!("name:    {}", a.name);
                println!("kind:    {}", a.kind);
                if !a.title.is_empty() {
                    println!("title:   {}", a.title);
                }
                println!(
                    "scope:   {}",
                    match &a.branch_id {
                        Some(bid) => format!("branch {bid}"),
                        None => "repo-shared".to_string(),
                    }
                );
                println!("rev:     {}", a.rev);
                println!("created: {}", a.created_at);
                println!("updated: {}", a.updated_at);
                return Ok(());
            }
            let version = match rev {
                Some(r) => artifact::version(&db, a.id, r).await?,
                None => artifact::latest_version(&db, a.id).await?,
            };
            match version {
                Some(v) => print!("{}", v.content),
                None => bail!(
                    "no revision {} of artifact '{}'",
                    rev.unwrap_or(a.rev),
                    a.name
                ),
            }
        }
    }
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

/// The live status of the branch working `issue`, as `"<branch> · <attention>
/// — <message>"`, or `None` when the issue is unclaimed or its branch row is
/// gone. This is what turns an issue lookup into a poll of a delegated sub-tree.
async fn working_branch_status(db: &db::Db, issue: &issue::Issue) -> Option<String> {
    let claimed = issue.claimed_branch.as_deref()?;
    let row = branch::find_by_repo_branch(db, &issue.repo_root, claimed)
        .await
        .ok()
        .flatten()?;
    // Best-effort display helper: on a read error show nothing rather than
    // fabricating a calm status.
    let attention = resolve_attention(db, &row.id).await.ok()?;
    let status = if row.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", row.description)
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
                    // The sub-agent wants the user when its `attention` tag is
                    // present with a loud value (`attention`/`blocked`); absence
                    // is the calm `ok` state.
                    let attention = resolve_attention(db, &row.id).await?;
                    if tags::ATTENTION_VALUES.contains(&attention.as_str()) {
                        let msg = if row.description.is_empty() {
                            attention
                        } else {
                            format!("{attention} — {}", row.description)
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

/// Read the `source` field a SessionStart hook receives as JSON on stdin
/// (`startup` | `resume` | `clear` | `compact`). Returns `None` when stdin is a
/// terminal (a human running the hook by hand), empty, or unparseable — callers
/// then fall back to the full-primer behaviour, which is always safe.
fn read_hook_source() -> Option<String> {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.read_to_string(&mut buf).ok()?;
    let v: Value = serde_json::from_str(buf.trim()).ok()?;
    v.get("source")?.as_str().map(str::to_owned)
}

/// The concise weaver re-orientation replayed after a context compaction: a
/// short reminder that this is still a weaver session, the `weaver summary`
/// catch-up, and the load-bearing rules an agent must not lose (status, no
/// blocking TUI prompts, PR-not-merge, close the tracking issue). The full guide
/// is one `weaver readme` away.
fn compact_replay(b: &branch::Branch, summary: &str) -> String {
    let summary = summary.trim_end();
    format!(
        "Context was just compacted — you are still in a **weaver session** on branch `{branch}` (a detached agent workstream in a git worktree; the user reviews asynchronously via the loom dashboard, not this terminal). Re-orientation:\n\n{summary}\n\nReminders: keep your status honest with `weaver set-status <ok|attention|blocked> \"<message>\"`; never block on an interactive TUI prompt — state the question as plain text and raise `weaver set-status attention`; finish by opening a PR (`gh pr create`) rather than merging, and `weaver issue close <id>` your tracking issue when the work is done. Run `weaver readme` for the full weaver workflow guide.\n",
        branch = b.branch,
    )
}

async fn cmd_hook(event: String) -> Result<()> {
    // Hooks must never break the agent: best-effort, swallow errors.
    let result: Result<()> = (async {
        let db = open_db().await?;
        let b = branch::resolve(&db).await?;
        // SessionStart carries a `source` on stdin (startup|resume|clear|compact);
        // we only read it for that event so other hooks don't touch stdin.
        let is_session_start = event == "session-start";
        let source = if is_session_start {
            read_hook_source()
        } else {
            None
        };
        let is_compact = source.as_deref() == Some("compact");
        events::record_local(
            &db,
            &b.id,
            "hook",
            json!({ "event": event, "source": source }),
        )
        .await?;
        if is_session_start {
            // After a compaction the agent has lost its working context but the
            // session is unchanged — replay a concise re-orientation (the
            // `weaver summary` catch-up) rather than the full WEAVER.md, which it
            // can pull back with `weaver readme` if it needs the full rules. On a
            // genuine start/resume/clear, inject the full primer.
            let context = if is_compact {
                let summary = render_summary(&db, &b).await.unwrap_or_default();
                compact_replay(&b, &summary)
            } else {
                weaver_md_for_branch(&b)
            };
            print!("{}", weaver_core::agent::session_primer(&context));
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
