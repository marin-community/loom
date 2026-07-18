//! weaver — the agent-facing CLI.
//!
//! An HTTP-only client of loom: every command drives the loom REST API
//! through `weaver-api::Client`, never a local database. The target server is
//! resolved from `$WEAVER_API` (or the address a local loom recorded while
//! serving), authenticated with `$LOOM_TOKEN` when set — the same resolution
//! `loom`'s own CLI uses (see `weaver_api::endpoint`). "Current branch"
//! resolves from `$WEAVER_BRANCH`, set by loom for every session it launches;
//! without it, or without a reachable loom, a command fails with a plain-text
//! error rather than falling back to any local state.

use anyhow::{anyhow, bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use weaver_api::{
    ArtifactUpsertReq, BranchView, Client, CreateIssueReq, CreateRepoIssueReq, IssueView,
    PatchIssueReq, ThreadDto,
};
use weaver_core::tags;

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
    Status {
        /// Attention level: `ok`, `attention`, or `blocked`. Omit to read.
        level: Option<String>,
        /// Current-state message, e.g. "Wired up routes; tests pass".
        message: Vec<String>,
    },
    /// Read, set, or clear a tag on a session.
    ///
    /// A tag is a single-valued `(key, value)` annotation on a branch with a
    /// one-line note and an author. The well-known loud keys are `attention`
    /// (the agent's own signal, normally written by `weaver status`) and `triage`
    /// (a watch's outside assessment); both accept `attention` or
    /// `blocked`. Any other key is free-form and quiet.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Print a quick orientation for the current branch.
    ///
    /// A one-shot catch-up for an agent picking up (or resuming) a branch: the
    /// goal, the current status, the outstanding tasks (this branch's open
    /// issues and any open sub-trees it delegated), and a line or two of hints
    /// for what to do next.
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
    /// Render the agent's conversation transcript as a markdown log. With no
    /// `--file`, locates the current worktree's transcript (Claude Code or
    /// Codex); `--json` prints the normalized iris format instead of markdown.
    Chatlog {
        /// Render this raw transcript file instead of locating one.
        #[arg(long)]
        file: Option<String>,
        /// Print the normalized iris JSON rather than rendered markdown.
        #[arg(long)]
        json: bool,
    },
    /// Record an agent hook event. Writes an `events` row; loom's monitor
    /// consumes it on its next tick.
    #[command(hide = true)]
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
    /// Label an issue with free-form `(key, value)` tags: set, rm, or ls.
    ///
    /// Issue tags are quiet annotations (priority, area, kind, …) rendered as
    /// pills in the loom Issues pane — there is no loud `attention`/`triage`
    /// ladder.
    Tag {
        #[command(subcommand)]
        cmd: IssueTagCmd,
    },
}

#[derive(Subcommand)]
enum IssueTagCmd {
    /// Set (insert or replace) a tag on an issue. The value must be non-empty;
    /// clear a label with `weaver issue tag rm`.
    Set {
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
    Rm { id: i64, key: String },
    /// List an issue's tags.
    Ls { id: i64 },
}

#[derive(Subcommand)]
enum ArtifactCmd {
    /// Write an artifact: append a new revision (creating it if absent). Reads
    /// `<file>`, or stdin when `<file>` is `-` or omitted.
    ///
    /// An image file (`.png`, `.jpg`, `.gif`, `.webp`, `.svg`, …; raster formats
    /// are also recognised from stdin by their magic bytes) is embedded as a
    /// base64 data-URI in a markdown wrapper, so it renders inline in loom — no
    /// need to hand-roll the data URI. A `.html`/`.htm` file is stored as the
    /// `html` kind, which loom renders in a sandboxed iframe.
    Write {
        /// The artifact name (its identity within the scope), e.g. `plan`.
        name: String,
        /// File to read the content from; `-` or omitted reads stdin.
        file: Option<String>,
        /// A human title for the artifact (envelope metadata).
        #[arg(long, default_value = "")]
        title: String,
        /// The content kind: `markdown` (the default; GFM + mermaid) or `html`
        /// (rendered in a sandboxed iframe). A `.html`/`.htm` file picks `html`
        /// on its own. Ignored for image files, which are always wrapped as
        /// markdown; any other value is stored verbatim and shown as source.
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
    /// Remove an artifact and its entire revision history. Resolves the name
    /// branch-scoped first, then repo-shared (what `show` would display); pass
    /// `--repo` to target the repo-shared one when a branch copy shadows it.
    Rm {
        /// The artifact name to remove.
        name: String,
        /// Remove the repo-shared artifact of this name, not the branch-scoped
        /// one.
        #[arg(long)]
        repo: bool,
    },
    /// Comment on an artifact: anchor a new discussion thread to a quoted
    /// span, or reply to an existing one.
    ///
    /// Without `--thread`, `--quote` is required and opens a new thread
    /// anchored to that text (plus optional `--prefix`/`--suffix` context for
    /// disambiguation), with `<body>` as its first comment. With `--thread
    /// <id>`, `<body>` is appended as the next reply. The CLI always comments
    /// as `agent` — the human side of the conversation comes through the API.
    Comment {
        /// The artifact name.
        name: String,
        /// Reply to this existing thread instead of starting a new one.
        #[arg(long)]
        thread: Option<i64>,
        /// The text the new thread anchors to. Required unless `--thread`.
        #[arg(long)]
        quote: Option<String>,
        /// A little context just before the quote, for disambiguation.
        #[arg(long, default_value = "")]
        prefix: String,
        /// A little context just after the quote, for disambiguation.
        #[arg(long, default_value = "")]
        suffix: String,
        /// The comment text. Joined with spaces.
        body: Vec<String>,
    },
    /// Resolve a discussion thread on an artifact.
    Resolve {
        /// The artifact name.
        name: String,
        /// The thread id (see `weaver artifact threads`).
        thread_id: i64,
    },
    /// List an artifact's discussion threads, each with its comments. Open
    /// threads only by default; `--all` also shows resolved/orphaned ones.
    Threads {
        /// The artifact name.
        name: String,
        /// Include resolved and orphaned threads too.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print one setting's value.
    Get { key: String },
    /// Set a setting.
    Set { key: String, value: String },
    /// Clear a setting, restoring its default.
    Rm { key: String },
    /// List every setting and its value.
    Ls,
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
        Cmd::Status { level, message } => cmd_status(level, message.join(" ")).await,
        Cmd::Tag { cmd } => cmd_tag(cmd).await,
        Cmd::Summary => cmd_summary().await,
        Cmd::Readme => cmd_readme().await,
        Cmd::Issue { cmd } => cmd_issue(cmd).await,
        Cmd::Artifact { cmd } => cmd_artifact(cmd).await,
        Cmd::Where => cmd_where().await,
        Cmd::Log { limit } => cmd_log(limit).await,
        Cmd::Chatlog { file, json } => cmd_chatlog(file, json),
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
// The loom client and "current branch" resolution
// ---------------------------------------------------------------------------

/// A client pointed at the server `$WEAVER_API` (or a local loom's recorded
/// address) resolves, authenticated with `$LOOM_TOKEN` when set.
fn client() -> Client {
    weaver_api::endpoint::default_client()
}

/// The branch key every command operates against: `$WEAVER_BRANCH`, set by
/// loom for every session it launches. There is no other way to identify
/// "the current branch" once the CLI no longer reads local git/db state —
/// without it, this only works as a bare client of a server that's told it
/// which branch it's fetching.
fn branch_key() -> Result<String> {
    let key = std::env::var("WEAVER_BRANCH").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        bail!(
            "not running inside a loom session ($WEAVER_BRANCH is not set) — \
             weaver only works inside a session loom launched"
        );
    }
    Ok(key.to_string())
}

/// The resolved attention level for a branch: the `attention` tag's value, or
/// `ok` when the branch carries no such tag (absence is calm).
fn attention_of(b: &BranchView) -> String {
    b.tags
        .iter()
        .find(|t| t.key == tags::ATTENTION_KEY)
        .map(|t| t.value.clone())
        .unwrap_or_else(|| "ok".to_string())
}

/// Read raw bytes from a file path, or stdin when `path` is `None` or `"-"`.
fn read_bytes_or_stdin(path: Option<&str>) -> Result<Vec<u8>> {
    use std::io::Read;
    match path {
        Some(p) if p != "-" => std::fs::read(p).map_err(|e| anyhow!("reading {p}: {e}")),
        _ => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .map_err(|e| anyhow!("reading stdin: {e}"))?;
            Ok(buf)
        }
    }
}

/// The largest image we embed inline. base64 inflates by ~⅓, and the data URI
/// rides in the artifact's content column / JSON views / SSE — a few MB is a
/// generous ceiling for a screenshot or diagram; past it, downscale first.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// The image MIME type for a filename's extension, or `None` if it isn't a
/// recognised image extension. Case-insensitive.
fn image_mime_from_ext(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// True when `filename` looks like a standalone HTML document (`.html`/`.htm`),
/// so a plain `weaver artifact write report report.html` lands as the `html`
/// kind loom renders in a sandboxed iframe — no `--kind html` needed. Only
/// promotes from the default `markdown`; an explicit `--kind` always wins.
fn is_html_ext(filename: Option<&str>) -> bool {
    filename
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
        .unwrap_or(false)
}

/// Sniff a raster image's MIME type from its leading magic bytes — for content
/// read from stdin, where there is no extension to go by. Only the unambiguous
/// binary formats are sniffed; text-ish SVG is recognised by extension alone, so
/// that markdown which merely embeds an `<svg>` is never mistaken for an image.
fn image_mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// If `bytes` is a recognised image (by `filename` extension, else raster magic
/// bytes), wrap it as a markdown document embedding it as a base64 data URI, so
/// it renders inline in loom with no binary storage. `None` means "not an image
/// — treat as text". `alt` is the image's alt text (the artifact title or name).
fn embed_image_markdown(alt: &str, filename: Option<&str>, bytes: &[u8]) -> Result<Option<String>> {
    use base64::Engine;
    let mime = filename
        .and_then(image_mime_from_ext)
        .or_else(|| image_mime_from_magic(bytes));
    let Some(mime) = mime else { return Ok(None) };
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!(
            "image is {:.1} MB; the inline limit is {} MB — downscale it first",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024)
        );
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let alt = if alt.is_empty() { "image" } else { alt };
    Ok(Some(format!("![{alt}](data:{mime};base64,{b64})\n")))
}

/// How many outstanding tasks `weaver summary` lists before collapsing the rest.
const SUMMARY_TASK_CAP: usize = 10;

/// Print a quick orientation for the current branch: the goal, the current
/// status, the outstanding tasks, and a hint or two for what to do next.
///
/// This is the catch-up an agent reads when it picks up a branch. It overlaps
/// `weaver status` (read), but where that shows an open-issue *count*, summary
/// lists the actual tasks and points at the next action.
async fn cmd_summary() -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let b = client.get_branch(&key).await?;
    print!("{}", render_summary(&client, &b).await?);
    Ok(())
}

/// Render the `weaver summary` catch-up as a string (see [`cmd_summary`]). Kept
/// separate from the printing so the post-compaction hook can replay the same
/// text into the agent's context as `additionalContext` (see [`cmd_hook`]).
async fn render_summary(client: &Client, b: &BranchView) -> Result<String> {
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
    let _ = writeln!(out, "Goal:    {goal}  (weaver artifact show goal)");

    let attention = attention_of(b);
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    let _ = writeln!(out, "Status:  {status}  (weaver status)");

    // Artifacts visible from this branch (its own + repo-shared) — the documents
    // the agent has written to weaver (designs, reports, the `plan`).
    let artifacts = client
        .list_branch_artifacts(&b.id, false)
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

    // Open discussion: unresolved comment threads across every artifact visible
    // from this branch — so a reviewer's feedback surfaces here even if the
    // agent never re-opens the artifact that carries it.
    let mut open_threads: Vec<(String, ThreadDto)> = Vec::new();
    for a in &artifacts {
        if let Ok(threads) = client.list_branch_threads(&b.id, &a.name).await {
            open_threads.extend(
                threads
                    .into_iter()
                    .filter(|t| t.status == "open")
                    .map(|t| (a.name.clone(), t)),
            );
        }
    }
    if !open_threads.is_empty() {
        open_threads.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
        out.push('\n');
        let _ = writeln!(out, "Open discussion ({}):", open_threads.len());
        for (name, t) in open_threads.iter().take(SUMMARY_TASK_CAP) {
            let _ = writeln!(
                out,
                "  #{} on {name}: \"{}\"  (weaver artifact threads {name})",
                t.id,
                truncate(&t.anchor.quote, 60)
            );
        }
        if open_threads.len() > SUMMARY_TASK_CAP {
            let _ = writeln!(
                out,
                "  (+{} more — weaver artifact threads <name>)",
                open_threads.len() - SUMMARY_TASK_CAP
            );
        }
    }

    // Outstanding work: this branch's own open issues, then any open sub-trees
    // it delegated (each carrying its sub-agent's live status). One repo-wide
    // fetch, partitioned client-side — the same data `issue ls` partitions.
    let issues = client
        .list_repo_issues(&b.repo_root, "repo", false)
        .await
        .unwrap_or_default();
    let open: Vec<&IssueView> = issues
        .iter()
        .filter(|i| i.claimed_branch.as_deref() == Some(b.branch.as_str()))
        .collect();
    let delegated: Vec<&IssueView> = issues
        .iter()
        .filter(|i| {
            i.source_branch.as_deref() == Some(b.branch.as_str())
                && i.claimed_branch.is_some()
                && i.claimed_branch.as_deref() != Some(b.branch.as_str())
        })
        .collect();
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
        for i in delegated
            .iter()
            .take(SUMMARY_TASK_CAP.saturating_sub(shown))
        {
            let claimed = i.claimed_branch.as_deref().unwrap_or("?");
            let who = match working_branch_status(client, &i.repo_root, claimed).await {
                Some(s) => s,
                None => claimed.to_string(),
            };
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
    let _ = writeln!(out, "Next steps:  (weaver log · weaver status)");
    let _ = writeln!(out, "  - {}", next_action_hint(&open, &delegated));
    Ok(out)
}

/// Print the full weaver workflow guide for this branch (the repo's own
/// `WEAVER.md` when it ships one, else the builtin). The same primer injected at
/// session start; `weaver readme` lets the agent pull it back on demand — most
/// usefully after a context compaction, when only the concise catch-up was
/// replayed.
async fn cmd_readme() -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let b = client.get_branch(&key).await?;
    print!("{}", weaver_md_for_branch(&b));
    Ok(())
}

/// A single suggested next action for `weaver summary`, derived from the open
/// work: pick up the first open task, else poll a delegated sub-tree, else
/// (nothing open) wrap up and open a PR.
fn next_action_hint(open: &[&IssueView], delegated: &[&IssueView]) -> String {
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

/// Ascend from `start` to the enclosing git worktree root (the directory holding
/// a `.git` entry — a dir in a normal clone, a file in a linked worktree).
/// Falls back to `start` when none is found, so a non-repo path still resolves.
fn worktree_root(start: &std::path::Path) -> std::path::PathBuf {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start.to_path_buf(),
        }
    }
}

/// Render the current worktree's (or a named file's) agent transcript. No
/// network access — pure filesystem, so it works whether or not loom is
/// reachable (the agent and its own transcript file are always co-located,
/// regardless of the isolation model).
fn cmd_chatlog(file: Option<String>, as_json: bool) -> Result<()> {
    use weaver_core::transcript;
    let log = match file {
        Some(path) => {
            let raw = std::fs::read_to_string(&path).map_err(|e| anyhow!("reading {path}: {e}"))?;
            transcript::parse(&raw)
                .ok_or_else(|| anyhow!("{path}: unrecognized transcript format"))?
        }
        None => {
            // Agents key their transcript off the worktree root (where the agent
            // was launched), so resolve that rather than the possibly-deeper cwd.
            let cwd = std::env::current_dir()?;
            let root = worktree_root(&cwd);
            let (_, files) = transcript::locate(&root)
                .ok_or_else(|| anyhow!("no agent transcript found for {}", root.display()))?;
            transcript::parse_files(&files)
                .ok_or_else(|| anyhow!("transcript found but could not be parsed"))?
        }
    };
    if as_json {
        println!("{}", log.to_json());
    } else {
        print!("{}", log.render_markdown());
    }
    Ok(())
}

async fn cmd_where() -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let b = client.get_branch(&key).await?;
    println!("repo:      {}", b.repo_root);
    println!("branch:    {}", b.branch);
    println!("base:      {}", b.base_branch);
    println!("branch-id: {}", b.id);
    Ok(())
}

async fn cmd_log(limit: i64) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let mut history = client.branch_log(&key).await?;
    history.truncate(limit.max(0) as usize);
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

async fn cmd_status(level: Option<String>, message: String) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    if let Some(level) = level {
        return cmd_status_write(&client, &key, &level, &message).await;
    }
    let b = client.get_branch(&key).await?;
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
    let attention = attention_of(&b);
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    println!("status:      {status}");
    println!("open issues: {}", b.open_issue_count);
    Ok(())
}

/// Report the agent's status: set the attention level and, when a message is
/// given, the accompanying current-state note. The level lives on the
/// `attention` tag — `ok` clears it (absence is the calm state), `attention`/
/// `blocked` set it. One call to loom (`POST /branches/{id}/status`), which
/// writes the description, sets or clears the tag, and records a single `tag`
/// event atomically server-side. An empty message leaves the previous message
/// in place — `weaver status ok` just lowers the level without wiping what the
/// agent last said.
async fn cmd_status_write(client: &Client, key: &str, level: &str, message: &str) -> Result<()> {
    let level = level.trim().to_ascii_lowercase();
    // `ok` is a valid *input* (return to calm) but is never stored — it clears
    // the tag. The two storable levels come from the tags registry. Checked
    // client-side too so a bad level fails fast, before any network round trip.
    if level != "ok" && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        bail!("unknown status '{level}' — expected one of ok, attention, blocked");
    }
    client.set_branch_status(key, &level, message).await?;
    let message = message.trim();
    if message.is_empty() {
        println!("status: {level}");
    } else {
        println!("status: {level} — {message}");
    }
    Ok(())
}

/// Resolve the branch a tag command targets: the named `--session` (an id,
/// `repo:branch`, or unambiguous prefix) when given, else the current branch.
async fn resolve_tag_target(
    client: &Client,
    current_key: &str,
    session: Option<&str>,
) -> Result<BranchView> {
    match session {
        Some(key) => client
            .get_branch(key)
            .await
            .map_err(|_| anyhow!("no session matching '{key}'")),
        None => client.get_branch(current_key).await,
    }
}

/// Set, clear, or list a tag on a branch. Tags unify the agent's `attention`
/// self-report and a watch's `triage` assessment with any free-form axis.
async fn cmd_tag(cmd: TagCmd) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    match cmd {
        TagCmd::Set {
            key: tag_key,
            value,
            note,
            session,
            by,
        } => {
            let target = resolve_tag_target(&client, &key, session.as_deref()).await?;
            let tag_key = tag_key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if !tags::is_valid_value(tag_key, value) {
                if tags::is_loud(tag_key) {
                    bail!(
                        "'{tag_key}' accepts only {} — use `weaver tag rm {tag_key}` to clear it",
                        tags::ATTENTION_VALUES.join(", ")
                    );
                }
                bail!("a tag value cannot be empty — use `weaver tag rm {tag_key}` to clear it");
            }
            client
                .set_branch_tag(&target.id, tag_key, value, note, by)
                .await?;
            if note.is_empty() {
                println!("tag: {} → {tag_key} = {value} (by {by})", target.branch);
            } else {
                println!(
                    "tag: {} → {tag_key} = {value} (by {by}) — {note}",
                    target.branch
                );
            }
        }
        TagCmd::Rm {
            key: tag_key,
            session,
        } => {
            let target = resolve_tag_target(&client, &key, session.as_deref()).await?;
            let tag_key = tag_key.trim();
            client
                .clear_branch_tag(&target.id, tag_key, "manual")
                .await?;
            println!("tag: {} → cleared {tag_key}", target.branch);
        }
        TagCmd::Ls { session } => {
            let target = resolve_tag_target(&client, &key, session.as_deref()).await?;
            if target.tags.is_empty() {
                println!("(no tags)");
                return Ok(());
            }
            for t in &target.tags {
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
    let client = client();
    let key = branch_key()?;
    let b = client.get_branch(&key).await?;
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
            let i = if repo {
                client
                    .create_repo_issue(&CreateRepoIssueReq {
                        repo_root: b.repo_root.clone(),
                        title: title.clone(),
                        body: body.unwrap_or_default(),
                        github_issue: github,
                        source_branch: Some(b.branch.clone()),
                    })
                    .await?
            } else {
                client
                    .create_branch_issue(
                        &b.id,
                        &CreateIssueReq {
                            title: title.clone(),
                            body: body.unwrap_or_default(),
                            github_issue: github,
                        },
                    )
                    .await?
            };
            println!("#{} {}", i.id, i.title);
        }
        IssueCmd::Ls {
            all,
            repo,
            mine,
            branch,
        } => {
            let target = branch.unwrap_or_else(|| b.branch.clone());
            let issues = client.list_repo_issues(&b.repo_root, "repo", all).await?;
            if repo {
                print_issue_ls_repo(&issues, &target);
            } else {
                print_issue_ls_default(&client, &issues, &target, mine).await;
            }
        }
        IssueCmd::Show { id } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, &b.repo_root)?;
            println!("#{} {}", i.id, i.title);
            println!("  status:  {}", i.status);
            println!(
                "  claimed: {}",
                i.claimed_branch.as_deref().unwrap_or("(backlog)")
            );
            // Surface the live status of the branch working this issue — what
            // makes `issue show` a poll of a delegated sub-tree, not just a
            // record lookup.
            if let Some(claimed) = &i.claimed_branch {
                if let Some(progress) = working_branch_status(&client, &i.repo_root, claimed).await
                {
                    println!("  working: {progress}");
                }
            }
            if let Some(src) = &i.source_branch {
                println!("  from:    {src}");
            }
            if let Some(n) = i.github_issue {
                println!("  github:  #{n}");
            }
            if !i.tags.is_empty() {
                let rendered = i
                    .tags
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
            cmd_issue_wait(
                &client,
                &b.repo_root,
                id,
                timeout,
                interval.max(1),
                closed_only,
            )
            .await?;
        }
        IssueCmd::Close { id } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, &b.repo_root)?;
            client
                .patch_issue(
                    id,
                    &PatchIssueReq {
                        status: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await?;
            println!("closed #{id}");
        }
        IssueCmd::Reopen { id } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, &b.repo_root)?;
            client
                .patch_issue(
                    id,
                    &PatchIssueReq {
                        status: Some("open".to_string()),
                        ..Default::default()
                    },
                )
                .await?;
            println!("reopened #{id}");
        }
        IssueCmd::Rm { id } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, &b.repo_root)?;
            client.delete_issue(id).await?;
            println!("removed #{id}");
        }
        IssueCmd::Tag { cmd } => cmd_issue_tag(&client, &b.repo_root, cmd).await?,
    }
    Ok(())
}

/// Set, clear, or list a free-form tag on an issue (`weaver issue tag …`).
async fn cmd_issue_tag(client: &Client, repo_root: &str, cmd: IssueTagCmd) -> Result<()> {
    match cmd {
        IssueTagCmd::Set {
            id,
            key,
            value,
            note,
            by,
        } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, repo_root)?;
            let key = key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if key.is_empty() {
                bail!("a tag key is required");
            }
            if value.is_empty() {
                bail!(
                    "a tag value cannot be empty — use `weaver issue tag rm {id} {key}` to clear it"
                );
            }
            client.set_issue_tag(id, key, value, note, by).await?;
            if note.is_empty() {
                println!("tag: #{id} → {key} = {value} (by {by})");
            } else {
                println!("tag: #{id} → {key} = {value} (by {by}) — {note}");
            }
        }
        IssueTagCmd::Rm { id, key } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, repo_root)?;
            client.clear_issue_tag(id, key.trim()).await?;
            println!("tag: #{id} → cleared {}", key.trim());
        }
        IssueTagCmd::Ls { id } => {
            let i = client.get_issue(id).await?;
            ensure_issue_in_repo(&i, repo_root)?;
            if i.tags.is_empty() {
                println!("(no tags)");
                return Ok(());
            }
            for t in &i.tags {
                let note = if t.note.is_empty() {
                    String::new()
                } else {
                    format!("  — {}", t.note)
                };
                println!("{} = {}{note}", t.key, t.value);
            }
        }
    }
    Ok(())
}

fn issue_line(i: &IssueView) -> String {
    let marker = if i.status == "open" { "[ ]" } else { "[x]" };
    let gh = i
        .github_issue
        .map(|n| format!(" (gh #{n})"))
        .unwrap_or_default();
    format!("#{:<4} {} {}{}", i.id, marker, i.title, gh)
}

/// Default `ls`: this branch's working set, plus the unclaimed repo backlog
/// (capped). `--mine` drops the backlog section. `issues` is one repo-wide
/// fetch (`all` already applied server-side); every section here is a
/// client-side partition of it.
async fn print_issue_ls_default(client: &Client, issues: &[IssueView], target: &str, mine: bool) {
    let working: Vec<&IssueView> = issues
        .iter()
        .filter(|i| i.claimed_branch.as_deref() == Some(target))
        .collect();
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
    let delegated: Vec<&IssueView> = issues
        .iter()
        .filter(|i| {
            i.source_branch.as_deref() == Some(target)
                && i.claimed_branch.is_some()
                && i.claimed_branch.as_deref() != Some(target)
        })
        .collect();
    if !delegated.is_empty() {
        println!("Delegated by this branch ({}):", delegated.len());
        for i in &delegated {
            let claimed = i.claimed_branch.as_deref().unwrap_or("?");
            let status = match working_branch_status(client, &i.repo_root, claimed).await {
                Some(s) => s,
                None => claimed.to_string(),
            };
            println!("  {}  → {status}", issue_line(i));
        }
        printed = true;
    }
    if !mine {
        let backlog: Vec<&IssueView> = issues
            .iter()
            .filter(|i| i.claimed_branch.is_none())
            .collect();
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
}

/// `ls --repo`: every open (or, with `--all`, every) issue in the repo, grouped
/// into this branch / unclaimed backlog / other branches.
fn print_issue_ls_repo(issues: &[IssueView], target: &str) {
    if issues.is_empty() {
        println!("(no issues)");
        return;
    }
    let mut mine = Vec::new();
    let mut backlog = Vec::new();
    let mut others = Vec::new();
    for i in issues {
        match i.claimed_branch.as_deref() {
            Some(b) if b == target => mine.push(i),
            Some(_) => others.push(i),
            None => backlog.push(i),
        }
    }
    let section = |title: String, items: &[&IssueView]| {
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
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

/// Read, write, and list artifacts — named, versioned documents stored in
/// loom. Scoped to the current branch by default; `--repo` is repo-shared.
async fn cmd_artifact(cmd: ArtifactCmd) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    match cmd {
        ArtifactCmd::Write {
            name,
            file,
            title,
            kind,
            repo,
        } => {
            let raw = read_bytes_or_stdin(file.as_deref())?;
            let alt = if title.trim().is_empty() {
                name.trim()
            } else {
                title.trim()
            };
            // An image is auto-wrapped as a base64 data-URI markdown doc (kind
            // forced to markdown so loom renders it inline); everything else is
            // stored as text under the requested kind. A `.html`/`.htm` file
            // promotes the default `markdown` to `html` (loom sandboxes it in an
            // iframe); an explicit `--kind` always wins.
            let (kind, content): (String, String) =
                match embed_image_markdown(alt, file.as_deref(), &raw)? {
                    Some(md) => ("markdown".to_string(), md),
                    None => {
                        let text = String::from_utf8(raw).map_err(|_| {
                            anyhow!(
                                "artifact content is not valid UTF-8 — \
                                 only text and image files are supported"
                            )
                        })?;
                        let kind = kind.trim();
                        let kind = if kind == "markdown" && is_html_ext(file.as_deref()) {
                            "html".to_string()
                        } else {
                            kind.to_string()
                        };
                        (kind, text)
                    }
                };
            let req = ArtifactUpsertReq {
                content,
                title: Some(title.trim().to_string()),
                kind: Some(kind),
                author: None,
                repo,
            };
            let view = client
                .write_branch_artifact(&key, name.trim(), &req)
                .await?;
            // The write already succeeded — loom is definitionally reachable at
            // this point, so the dashboard link is always known now (unlike the
            // direct-db days, when it depended on `$WEAVER_API`/`loom.json`
            // happening to be present). The server resolves the link so it
            // carries its externally-visible origin (`auth.base_url`, else the
            // request Host) — the loopback/wildcard `$WEAVER_API` we dialed
            // (often `http://0.0.0.0:7878`) is not a URL anyone can open. If that
            // resolution fails, fall back to the dialed base rather than lose the
            // rev line entirely.
            let url = client
                .branch_artifact_url(&key, &view.meta.name)
                .await
                .unwrap_or_else(|_| {
                    format!("{}/s/{key}/artifacts/{}", client.base(), view.meta.name)
                });
            let scope = if repo { "repo-shared" } else { "this branch" };
            println!("{url}  (rev {}, {scope})", view.meta.rev);
        }
        ArtifactCmd::Ls { repo } => {
            let artifacts = client.list_branch_artifacts(&key, repo).await?;
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
            let view = client
                .get_branch_artifact(&key, name.trim(), rev, false)
                .await?;
            if meta {
                println!("id:      {}", view.meta.id);
                println!("name:    {}", view.meta.name);
                println!("kind:    {}", view.meta.kind);
                if !view.meta.title.is_empty() {
                    println!("title:   {}", view.meta.title);
                }
                println!(
                    "scope:   {}",
                    match &view.meta.branch_id {
                        Some(bid) => format!("branch {bid}"),
                        None => "repo-shared".to_string(),
                    }
                );
                println!("rev:     {}", view.meta.rev);
                println!("created: {}", view.meta.created_at);
                println!("updated: {}", view.meta.updated_at);
                return Ok(());
            }
            print!("{}", view.content);
        }
        ArtifactCmd::Rm { name, repo } => {
            // Fetch first (branch-scoped resolution, matching `show`) so we can
            // report the scope/revision that got removed.
            let a = client
                .get_branch_artifact(&key, name.trim(), None, repo)
                .await
                .map_err(|_| anyhow!("no artifact '{}' — see `weaver artifact ls`", name.trim()))?;
            client
                .delete_branch_artifact(&key, name.trim(), repo)
                .await?;
            let scope = match &a.meta.branch_id {
                Some(bid) => format!("branch {bid}"),
                None => "repo-shared".to_string(),
            };
            println!("deleted {} ({scope}, was rev {})", a.meta.name, a.meta.rev);
        }
        ArtifactCmd::Comment {
            name,
            thread,
            quote,
            prefix,
            suffix,
            body,
        } => {
            let name = name.trim();
            let body = body.join(" ");
            if body.trim().is_empty() {
                bail!("a comment body is required");
            }
            match thread {
                Some(thread_id) => {
                    let c = client
                        .add_branch_thread_comment(&key, name, thread_id, &body)
                        .await?;
                    println!("added comment #{} to thread {thread_id} on {name}", c.seq);
                }
                None => {
                    let quote = quote.ok_or_else(|| {
                        anyhow!(
                            "--quote is required to start a new thread \
                             (or pass --thread <id> to reply)"
                        )
                    })?;
                    let a = client.get_branch_artifact(&key, name, None, false).await?;
                    let t = client
                        .create_branch_thread(
                            &key,
                            name,
                            a.meta.rev,
                            weaver_api::AnchorDto {
                                quote,
                                prefix,
                                suffix,
                            },
                            &body,
                        )
                        .await?;
                    println!("opened thread {} on {name} (rev {})", t.id, a.meta.rev);
                }
            }
        }
        ArtifactCmd::Resolve { name, thread_id } => {
            client
                .resolve_branch_thread(&key, name.trim(), thread_id)
                .await?;
            println!("resolved thread {thread_id} on {}", name.trim());
        }
        ArtifactCmd::Threads { name, all } => {
            let name = name.trim();
            let threads = client.list_branch_threads(&key, name).await?;
            let threads: Vec<_> = if all {
                threads
            } else {
                threads.into_iter().filter(|t| t.status == "open").collect()
            };
            if threads.is_empty() {
                let scope = if all { "" } else { "open " };
                println!("(no {scope}threads on {name})");
                return Ok(());
            }
            for t in &threads {
                println!(
                    "#{} [{}] \"{}\"",
                    t.id,
                    t.status,
                    truncate(&t.anchor.quote, 70)
                );
                for c in &t.comments {
                    println!("    {}: {}", c.author, c.body);
                }
            }
        }
    }
    Ok(())
}

/// Confirm an issue exists and lives in `repo_root`. Cross-*repo* access is the
/// real mistake to guard; within a repo, claimed and backlog items are all fair
/// game.
fn ensure_issue_in_repo(i: &IssueView, repo_root: &str) -> Result<()> {
    if i.repo_root != repo_root {
        bail!("issue #{} belongs to a different repo", i.id);
    }
    Ok(())
}

/// The live status of the branch working an issue, as `"<branch> · <attention>
/// — <message>"`, or `None` when the branch row can't be resolved (a stale
/// `claimed_branch` name, or a network hiccup — best-effort). This is what
/// turns an issue lookup into a poll of a delegated sub-tree.
async fn working_branch_status(client: &Client, repo_root: &str, claimed: &str) -> Option<String> {
    let key = format!("{repo_root}:{claimed}");
    let b = client.get_branch(&key).await.ok()?;
    let attention = attention_of(&b);
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    Some(format!("{claimed} · {status}"))
}

/// Block until issue `id` finishes (closes) or — unless `closed_only` — its
/// claiming branch raises attention above `ok`. Polls every `interval` seconds;
/// exits the process non-zero if `timeout` (when non-zero) elapses first.
async fn cmd_issue_wait(
    client: &Client,
    repo_root: &str,
    id: i64,
    timeout: u64,
    interval: u64,
    closed_only: bool,
) -> Result<()> {
    let issue = client.get_issue(id).await?;
    ensure_issue_in_repo(&issue, repo_root)?;
    if issue.status != "open" {
        println!("issue #{id} is {} — nothing to wait for", issue.status);
        return Ok(());
    }
    match issue.claimed_branch.as_deref() {
        Some(claimed) => match working_branch_status(client, repo_root, claimed).await {
            Some(s) => println!("waiting on #{id} ({}) — {s}", issue.title),
            None => println!("waiting on #{id} ({})", issue.title),
        },
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
        let cur = client.get_issue(id).await?;
        if cur.status != "open" {
            println!("issue #{id} closed — sub-tree finished");
            return Ok(());
        }
        if !closed_only {
            if let Some(name) = cur.claimed_branch.as_deref() {
                let key = format!("{repo_root}:{name}");
                if let Ok(row) = client.get_branch(&key).await {
                    // The sub-agent wants the user when its `attention` tag is
                    // present with a loud value (`attention`/`blocked`); absence
                    // is the calm `ok` state.
                    let attention = attention_of(&row);
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
            let progress = match cur.claimed_branch.as_deref() {
                Some(c) => working_branch_status(client, repo_root, c)
                    .await
                    .unwrap_or_else(|| "open".to_string()),
                None => "open".to_string(),
            };
            bail!("timed out after {timeout}s — #{id} still open ({progress})");
        }
    }
}

/// The WEAVER.md to inject at session start: the repo's own copy when it ships
/// one, else the builtin. We look in the worktree the hook is actually running
/// in (its cwd at launch is the worktree root) and then in the primary checkout,
/// so a `WEAVER.md` committed on the base branch is picked up either way.
fn weaver_md_for_branch(branch: &BranchView) -> String {
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
fn compact_replay(b: &BranchView, summary: &str) -> String {
    let summary = summary.trim_end();
    format!(
        "Context was just compacted — you are still in a **weaver session** on branch `{branch}` (a detached agent workstream in a git worktree; the user reviews asynchronously via the loom dashboard, not this terminal). Re-orientation:\n\n{summary}\n\nReminders: keep your status honest with `weaver status <ok|attention|blocked> \"<message>\"`; never block on an interactive TUI prompt — state the question as plain text and raise `weaver status attention`; finish by opening a PR (`gh pr create`) rather than merging, and `weaver issue close <id>` your tracking issue when the work is done. Run `weaver readme` for the full weaver workflow guide.\n",
        branch = b.branch,
    )
}

async fn cmd_hook(event: String) -> Result<()> {
    // Hooks must never break the agent: best-effort, swallow errors.
    let result: Result<()> = (async {
        let client = client();
        let key = branch_key()?;
        // SessionStart carries a `source` on stdin (startup|resume|clear|compact);
        // we only read it for that event so other hooks don't touch stdin.
        let is_session_start = event == "session-start";
        let source = if is_session_start {
            read_hook_source()
        } else {
            None
        };
        let is_compact = source.as_deref() == Some("compact");
        client
            .record_branch_event(&key, "hook", json!({ "event": event, "source": source }))
            .await?;
        if is_session_start {
            // After a compaction the agent has lost its working context but the
            // session is unchanged — replay a concise re-orientation (the
            // `weaver summary` catch-up) rather than the full WEAVER.md, which it
            // can pull back with `weaver readme` if it needs the full rules. On a
            // genuine start/resume/clear, inject the full primer.
            let b = client.get_branch(&key).await?;
            let context = if is_compact {
                let summary = render_summary(&client, &b).await.unwrap_or_default();
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
    let client = client();
    match cmd {
        ConfigCmd::Ls => {
            let settings = client.list_settings().await?;
            for s in &settings.settings {
                let suffix = if s.is_default { "  (default)" } else { "" };
                println!("{} = {}{suffix}", s.key, s.value);
            }
        }
        ConfigCmd::Get { key } => {
            let settings = client.list_settings().await?;
            match settings.settings.iter().find(|s| s.key == key) {
                Some(s) => println!("{}", s.value),
                None => bail!("no setting '{key}' — see `weaver config ls`"),
            }
        }
        ConfigCmd::Set { key, value } => {
            let mut changes = serde_json::Map::new();
            changes.insert(key.clone(), json!(value));
            client.patch_settings(changes).await?;
            println!("set {key}");
        }
        ConfigCmd::Rm { key } => {
            let mut changes = serde_json::Map::new();
            changes.insert(key.clone(), Value::Null);
            client.patch_settings(changes).await?;
            println!("removed {key}");
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

    #[test]
    fn image_extensions_map_to_mime_case_insensitively() {
        assert_eq!(image_mime_from_ext("shot.png"), Some("image/png"));
        assert_eq!(image_mime_from_ext("./a/b/Photo.JPG"), Some("image/jpeg"));
        assert_eq!(image_mime_from_ext("logo.svg"), Some("image/svg+xml"));
        assert_eq!(image_mime_from_ext("anim.webp"), Some("image/webp"));
        // Not images: plain docs and extension-less names.
        assert_eq!(image_mime_from_ext("design.md"), None);
        assert_eq!(image_mime_from_ext("plan"), None);
    }

    #[test]
    fn html_extensions_are_recognised_case_insensitively() {
        assert!(is_html_ext(Some("report.html")));
        assert!(is_html_ext(Some("./out/Dashboard.HTM")));
        // Not HTML: other docs, extension-less names, and stdin (no filename).
        assert!(!is_html_ext(Some("plan.md")));
        assert!(!is_html_ext(Some("notes")));
        assert!(!is_html_ext(None));
    }

    #[test]
    fn raster_magic_bytes_are_sniffed_but_text_is_not() {
        assert_eq!(
            image_mime_from_magic(b"\x89PNG\r\n\x1a\n....."),
            Some("image/png")
        );
        assert_eq!(
            image_mime_from_magic(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some("image/jpeg")
        );
        assert_eq!(image_mime_from_magic(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            image_mime_from_magic(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        // Markdown that merely contains an <svg> is text, never sniffed as image.
        assert_eq!(image_mime_from_magic(b"# Notes\n<svg>...</svg>\n"), None);
    }

    #[test]
    fn embed_wraps_an_image_and_passes_text_through() {
        // A PNG by extension → a markdown image with a base64 data URI + alt text.
        let png = b"\x89PNG\r\n\x1a\nzzzz";
        let md = embed_image_markdown("My shot", Some("shot.png"), png)
            .unwrap()
            .expect("png embeds");
        assert!(md.starts_with("![My shot](data:image/png;base64,"));
        assert!(md.trim_end().ends_with(')'));

        // Extension-less stdin still embeds via magic bytes; empty alt → "image".
        let md = embed_image_markdown("", None, png)
            .unwrap()
            .expect("magic embeds");
        assert!(md.starts_with("![image](data:image/png;base64,"));

        // SVG (text) embeds by extension so it renders as an image, not source.
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'></svg>";
        let md = embed_image_markdown("d", Some("d.svg"), svg)
            .unwrap()
            .expect("svg embeds");
        assert!(md.starts_with("![d](data:image/svg+xml;base64,"));

        // Non-image content is left for the text path.
        assert!(embed_image_markdown("notes", Some("notes.md"), b"# Hi")
            .unwrap()
            .is_none());
        assert!(embed_image_markdown("notes", None, b"plain text")
            .unwrap()
            .is_none());
    }

    #[test]
    fn embed_rejects_an_oversized_image() {
        let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(embed_image_markdown("x", Some("x.png"), &big).is_err());
    }
}
