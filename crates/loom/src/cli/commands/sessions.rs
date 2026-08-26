//! `loom sessions` — the uniform way to drive a child session, plus the
//! `loom launch` / `ps` / `attach` shortcuts that share its options.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};

use crate::agent_cli::TagCmd as AgentTagCmd;
use crate::cli::support::{configure_agent_client, truncate};
use crate::client::{self, Client};
use weaver_api::operations::{branches, sessions};
use weaver_api::{
    SearchSessionsOptions, SessionCreatorFilter, SessionSearchAttention, SessionSearchStatus,
    SessionView,
};

use super::layout::{run_session_layout, SessionLayoutCmd};

#[derive(Args)]
pub struct AttachArgs {
    pub session: String,
}

/// Subcommands under `loom sessions` — the uniform way to drive a child session.
// `Launch` carries the flattened `LaunchOpts` arg struct, which clap derives
// against by value — boxing it (clippy's `large_enum_variant` suggestion) would
// fight the `Subcommand` derive. This is a short-lived CLI dispatch enum, so the
// size skew is harmless.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum SessionCmd {
    /// Launch a new session: worktree + terminal + agent, seeded with a task.
    ///
    /// The positional argument is the task the agent should work on — it
    /// becomes the branch goal and the agent's opening prompt:
    ///
    ///     loom sessions launch "Add a /health endpoint and a test for it"
    ///
    /// The branch name (`weaver/<slug>`) is derived from the task; override it
    /// with `--name`. To pick up existing work instead of describing a new
    /// task, use `--claim <id>`, `--issue <n>`, or `--branch <name>`.
    Launch(LaunchOpts),
    /// Print a session's dashboard URL — the link to hand a human.
    ///
    /// With no argument this is *your own* session (resolved from
    /// `$WEAVER_BRANCH`), so an agent opening a PR can link back to the session
    /// that produced it:
    ///
    ///     gh pr create --body "$(printf 'Fixes #12\n\nloom: %s\n' "$(loom sessions url)")"
    ///
    /// The URL is resolved by the server, which is the only thing that knows
    /// loom's externally-visible address — building it from `$WEAVER_API` inside
    /// a session would yield a loopback link nobody else can open.
    Url {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        /// Defaults to the current session.
        session: Option<String>,
    },
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
    /// Deliver a message to a session now. ACP sessions stop a live turn and
    /// start the message as a new turn.
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
    /// Interrupt a session's current turn (sends Escape to terminal sessions).
    #[command(visible_alias = "break")]
    Interrupt {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
    },
    /// Print a session's recent terminal screen.
    Preview {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Extra scrollback lines above the visible screen (0 = visible only).
        #[arg(long, default_value = "0")]
        lines: usize,
    },
    /// Print the typed, bounded worktree changes relative to the branch base.
    Changes {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
    },
    /// Read, set, or remove free-form session tags.
    Tags {
        #[command(subcommand)]
        cmd: AgentTagCmd,
    },
    /// Print recent durable events (defaults to the current session).
    Events {
        /// Session key; omit for the session containing this command.
        session: Option<String>,
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Render this worktree's local agent transcript without contacting Loom.
    Transcript {
        /// Render a specific raw Claude or Codex transcript file.
        #[arg(long)]
        file: Option<String>,
        /// Print normalized iris JSON instead of Markdown.
        #[arg(long)]
        json: bool,
    },
    /// List active sessions (also `loom ps`).
    ///
    /// Archived (torn-down) sessions are hidden by default — pass `--archived`
    /// to include them. Successful automation sessions are normal rows.
    /// `--search <text>` spans placement, title/prompt, repo/branch, issue/PR,
    /// tags, status, profile, and provenance. The list is an index: it shows
    /// each session's id, lifecycle, attention, location, and title — pull the
    /// full detail for one with `loom sessions get <id>`.
    #[command(name = "list", alias = "ls")]
    Ls {
        /// Include archived (torn-down) sessions.
        #[arg(long)]
        archived: bool,
        /// Deprecated compatibility flag; automation sessions are included.
        #[arg(long, hide = true)]
        automation: bool,
        /// Include engine-managed watch sessions (admin only; implies automation).
        #[arg(long)]
        managed: bool,
        /// Case-insensitive substring filter over title / branch / goal.
        #[arg(long)]
        search: Option<String>,
        /// Filter the typed lifecycle state.
        #[arg(long)]
        status: Option<SessionSearchStatus>,
        /// Filter the resolved attention state.
        #[arg(long)]
        attention: Option<SessionSearchAttention>,
        /// Filter by who launched work: mine, ops, mine-and-ops, or other-users.
        #[arg(long)]
        creator: Option<SessionCreatorFilter>,
    },
    /// Read or edit the durable Spaces → Groups → Sessions workbench layout.
    Layout {
        #[command(subcommand)]
        cmd: SessionLayoutCmd,
    },
    /// Rename a session: set the one-line title shown on the dashboard.
    Rename {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// The new title. Multiple words are joined, so quoting is optional.
        title: Vec<String>,
    },
    /// Ask the session's bounded metadata helper to refresh an eligible task label.
    RegenerateTitle {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
    },
    /// Enable or disable automatic generated task labels for one session.
    TitleGeneration {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Whether generated title refreshes are enabled.
        #[arg(
            value_parser = clap::value_parser!(bool),
            action = clap::ArgAction::Set
        )]
        enabled: bool,
    },
    /// Read the cached resumption cue, optionally ensuring one now.
    Cue {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Generate when inactivity says a cue is due.
        #[arg(long)]
        ensure: bool,
        /// Explicitly generate regardless of the inactivity threshold.
        #[arg(long)]
        force: bool,
    },
    /// Get one session's details.
    #[command(name = "get", alias = "show")]
    Show { session: String },
    /// Attach your terminal to a session (also `loom attach`).
    Attach { session: String },
    /// Archive a session or failed launch: tear down runtime, keep history.
    ///
    /// An unmatched automation launch is addressed by its reserved session id,
    /// the same id shown in the Interventions section/API.
    Archive { session: String },
    /// Recreate the terminal session for an orphaned session.
    Adopt { session: String },
    /// Recover a session: restart a failed ACP runtime, or rebuild an archive.
    Recover { session: String },
    /// Replace the provider behind a live ACP session, preserving its worktree
    /// and canonical conversation journal.
    Handoff {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Target launch profile. When present, Loom previews a canonical
        /// profile selection and sends both optimistic revisions.
        #[arg(long)]
        profile: Option<String>,
        /// Target ACP agent runtime (for example `claude` or `codex`). With
        /// `--profile` this is a one-handoff override; without it this selects
        /// the legacy runtime-only compatibility path.
        #[arg(long)]
        agent: Option<String>,
        /// Target model selector; omit for the runtime default.
        #[arg(long)]
        model: Option<String>,
        /// Target reasoning effort; omit for the runtime default.
        #[arg(long)]
        effort: Option<String>,
        /// Target ACP permission posture; omit to keep the session's stamped mode.
        #[arg(long)]
        mode: Option<String>,
    },
    /// Remove a session or unmatched launch attempt and its runtime.
    Rm {
        session: String,
        #[arg(long)]
        keep_branch: bool,
    },
}

/// Shared `launch` options, used by both `loom sessions launch` and the
/// top-level `loom launch` shortcut.
#[derive(Args)]
pub struct LaunchOpts {
    /// What the agent should do. Sets the branch goal and is fed to the agent as
    /// its first prompt. Multiple words are joined, so quoting is optional. Omit
    /// only when seeding from `--claim`/`--issue`/`--branch`.
    task: Vec<String>,
    /// Named launch profile. Defaults to `default`.
    #[arg(long)]
    profile: Option<String>,
    /// Branch slug to create (`weaver/<name>`). Defaults to a slug derived from
    /// the task. Mutually exclusive with `--branch`.
    #[arg(long)]
    name: Option<String>,
    /// Agent to run. Optional — omit to use the selected profile's agent.
    #[arg(long)]
    agent: Option<String>,
    /// Repo to launch into: either a path to (any directory inside) a local
    /// checkout, or a GitHub `owner/name` slug (or clone URL) — a repo loom
    /// doesn't have yet is cloned into its managed repo store on first use. The
    /// new worktree is cut from the repo's mainline. Defaults to the current
    /// directory — so without it you launch into whatever repo you happen to be
    /// standing in, which is the wrong one when you mean another.
    #[arg(long)]
    repo: Option<String>,
    /// Branch to fork the new worktree from. Defaults to a freshly-fetched
    /// `origin/<default branch>` (the repo's mainline). New work starts from the
    /// latest upstream.
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
    /// Claim an existing Loom issue (by id) for this session: seeds the goal
    /// from it and moves it out of the repo backlog.
    #[arg(long)]
    claim: Option<i64>,
    /// Resume an existing branch. Mutually exclusive with `--name`.
    #[arg(long)]
    branch: Option<String>,
    /// Model selector accepted by the selected agent. Omit to use the selected
    /// agent's default.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort: low, medium, high, xhigh, or max. Omit to use the
    /// selected agent's default.
    #[arg(long)]
    effort: Option<String>,
    /// Execution backend: `terminal` forces the PTY fallback for a builtin;
    /// `acp` opts in explicitly. Omit to use the agent's default (acp for the
    /// builtins).
    #[arg(long)]
    protocol: Option<String>,
    /// ACP launch permission posture: `auto`, `bypassPermissions`, `acceptEdits`,
    /// `default`, or `plan`. Omit to use the selected profile's mode; ignored
    /// for a terminal launch.
    #[arg(long)]
    mode: Option<String>,
}

/// Dispatch the `loom sessions <verb>` subcommands.
pub async fn run_session(cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::Launch(opts) => cmd_launch(opts.into()).await,
        SessionCmd::Url { session } => cmd_session_url(session).await,
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
        SessionCmd::Interrupt { session } => cmd_session_interrupt(session).await,
        SessionCmd::Preview { session, lines } => cmd_session_preview(session, lines).await,
        SessionCmd::Changes { session } => {
            let changes = client::default()?
                .invoke::<sessions::changes::Op>(&sessions::changes::Input {
                    session: session.to_string(),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&changes)?);
            Ok(())
        }
        SessionCmd::Tags { cmd } => {
            configure_agent_client()?;
            crate::agent_cli::run_tag(cmd).await
        }
        SessionCmd::Events { session, limit } => {
            if let Some(session) = session {
                let events = client::default()?
                    .invoke::<branches::events::list::Op>(&branches::events::list::Input {
                        branch: session.to_string(),
                    })
                    .await?;
                for event in events.into_iter().rev().take(limit.max(0) as usize).rev() {
                    println!(
                        "{}  {:<14} {}",
                        event.created_at,
                        event.kind,
                        serde_json::to_string(&event.data)?
                    );
                }
                Ok(())
            } else {
                configure_agent_client()?;
                crate::agent_cli::run_events(limit).await
            }
        }
        SessionCmd::Transcript { file, json } => crate::agent_cli::run_chatlog(file, json),
        SessionCmd::Ls {
            archived,
            automation: _,
            managed,
            search,
            status,
            attention,
            creator,
        } => {
            cmd_ps(PsOptions {
                archived,
                managed,
                search,
                status,
                attention,
                creator,
            })
            .await
        }
        SessionCmd::Layout { cmd } => run_session_layout(cmd).await,
        SessionCmd::Rename { session, title } => cmd_session_rename(session, title.join(" ")).await,
        SessionCmd::RegenerateTitle { session } => cmd_session_regenerate_title(session).await,
        SessionCmd::TitleGeneration { session, enabled } => {
            cmd_session_title_generation(session, enabled).await
        }
        SessionCmd::Cue {
            session,
            ensure,
            force,
        } => cmd_session_cue(session, ensure || force, force).await,
        SessionCmd::Show { session } => cmd_show(session).await,
        SessionCmd::Attach { session } => cmd_attach(session).await,
        SessionCmd::Archive { session } => cmd_archive(session).await,
        SessionCmd::Adopt { session } => cmd_adopt(session).await,
        SessionCmd::Recover { session } => cmd_recover(session).await,
        SessionCmd::Handoff {
            session,
            profile,
            agent,
            model,
            effort,
            mode,
        } => cmd_handoff(session, profile, agent, model, effort, mode).await,
        SessionCmd::Rm {
            session,
            keep_branch,
        } => cmd_rm(session, keep_branch).await,
    }
}

/// The agent's resolved attention level from a `SessionView`'s `branch.tags` —
/// the value of the `attention` tag, or `ok` when it is absent (the calm state).
pub fn branch_attention(ws: &SessionView) -> &str {
    ws.branch
        .tags
        .iter()
        .find(|t| t.key == "attention")
        .map(|t| t.value.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("ok")
}

/// Parsed launch inputs, after folding the positional task words into a single
/// `goal` string.
pub struct LaunchArgs {
    goal: String,
    profile: Option<String>,
    name: Option<String>,
    agent: Option<String>,
    repo: Option<String>,
    base: Option<String>,
    title: Option<String>,
    issue: Option<i64>,
    claim: Option<i64>,
    branch: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    protocol: Option<String>,
    mode: Option<String>,
}

impl From<LaunchOpts> for LaunchArgs {
    fn from(o: LaunchOpts) -> Self {
        LaunchArgs {
            goal: o.task.join(" "),
            profile: o.profile,
            name: o.name,
            agent: o.agent,
            repo: o.repo,
            base: o.base,
            title: o.title,
            issue: o.issue,
            claim: o.claim,
            branch: o.branch,
            model: o.model,
            effort: o.effort,
            protocol: o.protocol,
            mode: o.mode,
        }
    }
}

/// A bare `loom sessions launch` with nothing to work on — no task, no name, no title,
/// and nothing to pick up (`--claim`/`--issue`/`--branch`). Launching anyway
/// would spawn an agent with an empty goal that "starts unprompted", so we
/// stop and point the user at the useful forms instead.
pub fn launch_underspecified(a: &LaunchArgs) -> bool {
    a.goal.trim().is_empty()
        && a.name.is_none()
        && a.title.is_none()
        && a.issue.is_none()
        && a.claim.is_none()
        && a.branch.is_none()
}

pub const LAUNCH_HINT: &str = "nothing to do — give the agent a task or something to pick up:
  loom sessions launch \"<what the agent should do>\"  # the common case
  loom sessions launch --claim <id>                    # pick up a Loom issue
  loom sessions launch --issue <n>                     # seed from a GitHub issue
  loom sessions launch --branch <name>                 # resume an existing branch
  loom sessions launch --name <slug> --agent shell     # an empty named worktree (no task)
See `loom sessions launch --help` for all options.";

/// What a launch forks from, once `--repo` has been classified.
#[derive(Debug, PartialEq, Eq)]
pub enum RepoTarget {
    /// A local checkout — any directory inside it. The server resolves the repo
    /// from this path (its main worktree), so it travels as the request's `cwd`.
    Local(std::path::PathBuf),
    /// A repo loom manages for us: a GitHub `owner/name` slug or a clone URL.
    /// Travels as the request's `repo`, which the server registers and clones
    /// into its managed store on first use.
    Managed(String),
}

/// Classify `--repo` (absent → the current directory). An existing path is a
/// local checkout; anything else is a managed-repo reference if it parses as a
/// clean `owner/name` slug or clone URL — which is what lets you launch into a
/// repo this machine has never checked out. Neither one is a typo, and saying so
/// here beats an opaque server-side failure.
///
/// A path that exists wins over a slug of the same spelling: a real directory in
/// front of you is never a guess, so `--repo ./acme/widgets` can't be hijacked
/// into a clone of `github.com/acme/widgets`.
pub fn resolve_repo_target(repo: Option<&str>) -> Result<RepoTarget> {
    let Some(input) = repo.map(str::trim).filter(|s| !s.is_empty()) else {
        let cwd = std::env::current_dir().context("could not read the current directory")?;
        return Ok(RepoTarget::Local(cwd));
    };
    // Canonicalizing anchors a relative path to the CLI's cwd, not the daemon's.
    if let Ok(path) = std::path::Path::new(input).canonicalize() {
        return Ok(RepoTarget::Local(path));
    }
    if crate::repo::parse_slug(input).is_ok() {
        return Ok(RepoTarget::Managed(input.to_string()));
    }
    bail!(
        "--repo '{input}' is neither a local path that exists nor a repo to clone \
         (expected `owner/name` or a clone URL)"
    )
}

pub async fn cmd_launch(a: LaunchArgs) -> Result<()> {
    if launch_underspecified(&a) {
        bail!("{LAUNCH_HINT}");
    }
    let LaunchArgs {
        goal,
        profile,
        name,
        agent,
        repo,
        base,
        title,
        issue,
        claim,
        branch,
        model,
        effort,
        protocol,
        mode,
    } = a;
    let client = client::default()?;
    let target = resolve_repo_target(repo.as_deref())?;
    // A managed repo travels as `repo` (the server registers it and clones it if
    // this is its first use); a local checkout travels as `cwd`. Exactly one is
    // set — the server ignores `cwd` whenever `repo` is present.
    let (cwd, managed_repo) = match target {
        RepoTarget::Local(path) => (path.display().to_string(), None),
        RepoTarget::Managed(repo) => (String::new(), Some(repo)),
    };
    if let Some(r) = managed_repo.as_deref() {
        println!("repo {r} — cloning it if loom doesn't have it yet...");
    }
    // When an agent in a Loom session runs `loom sessions launch`,
    // `$WEAVER_BRANCH` is its own branch id — pass it so the tracking issue is
    // attributed to the launching (parent) agent. A human shell launch leaves it
    // unset.
    let parent_branch = std::env::var("WEAVER_BRANCH")
        .ok()
        .filter(|s| !s.is_empty());
    let selection = weaver_api::LaunchSelection {
        profile: profile.unwrap_or_else(|| "default".to_string()),
        overrides: weaver_api::LaunchOverrides {
            agent,
            model,
            effort,
            protocol,
            mode,
            ..Default::default()
        },
    };
    let preview = client
        .invoke::<sessions::launches::resolve::Op>(&sessions::launches::resolve::Input {
            selection: selection.clone(),
        })
        .await?;
    if !preview.valid {
        bail!(
            "launch settings are not currently valid:\n{}",
            preview.errors.join("\n")
        );
    }
    let ws = client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            title: title.clone(),
            goal: (Some(goal)).clone(),
            repo: managed_repo.clone(),
            cwd: cwd.clone(),
            base: base.clone(),
            claim_issue: claim,
            issue,
            parent_branch: parent_branch.clone(),
            name: name.clone(),
            existing_branch: branch.clone(),
            selection: (Some(selection)).clone(),
            expected_profile_revision: (Some(preview.profile_revision)),
            expected_resolver_revision: (Some(preview.resolver_revision)).clone(),
            ..Default::default()
        })
        .await?;
    let id = &ws.id;
    println!("launched session {id}  ({})", ws.branch.name);
    println!("  title:  {}", ws.branch.title);
    let g = &ws.branch.goal;
    println!(
        "  goal:   {}",
        if g.is_empty() {
            "(none — agent started unprompted)"
        } else {
            g
        }
    );
    println!("  branch: {}", ws.branch.branch);
    if !ws.model.is_empty() {
        println!("  model:  {}", ws.model);
    }
    if !ws.effort.is_empty() {
        println!("  effort: {}", ws.effort);
    }
    println!("  dir:    {}", ws.work_dir);
    println!("  channel: {id}  (loom channels read --channel {id} | wait --channel {id})");
    if let Some(n) = ws.tracking_issue {
        // Explicit claimed/imported work items remain attached while ordinary
        // coordination uses the session channel above.
        println!("  work:   Loom issue #{n}  (explicit backlog/external mapping)");
    }
    println!("  attach: loom attach {id}");
    Ok(())
}

/// Resolve a session view by key, surfacing a clearer error than a bare 404 when
/// the key matches no live session.
pub async fn fetch_session(client: &Client, key: &str) -> Result<SessionView> {
    client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: key.to_string(),
        })
        .await
        .with_context(|| format!("no live session for '{key}'"))
}

/// One-line attention summary: the resolved level (the agent's `attention` tag,
/// `ok` when absent), plus its current-state message when set.
pub fn attention_summary(ws: &SessionView) -> String {
    let attention = branch_attention(ws);
    let message = &ws.branch.description;
    if message.is_empty() {
        attention.to_string()
    } else {
        format!("{attention} — {message}")
    }
}

/// `loom sessions url` — print a session's dashboard URL, defaulting to the
/// session we are running inside. The server resolves the URL (only it knows
/// loom's public origin); this just prints it bare, so it composes into a
/// `gh pr create --body "$(…)"` without any trimming.
pub async fn cmd_session_url(key: Option<String>) -> Result<()> {
    let key = match key {
        Some(k) => k,
        // `$WEAVER_BRANCH` is the branch id loom exports into every session it
        // launches, and the API resolves a branch id as a session key.
        None => std::env::var("WEAVER_BRANCH")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .context(
                "not inside a loom session ($WEAVER_BRANCH is not set) — \
                 pass a session key explicitly: loom sessions url <session>",
            )?,
    };
    let client = client::default()?;
    let res = client
        .invoke::<sessions::url::Op>(&sessions::url::Input {
            session: key.clone(),
        })
        .await
        .with_context(|| format!("no live session for '{key}'"))?;
    println!("{}", res.url);
    Ok(())
}

/// `loom sessions poll` — a one-shot status read: lifecycle + attention.
pub async fn cmd_session_poll(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = fetch_session(&client, &key).await?;
    println!("session {}  ({})", ws.id, ws.branch.name);
    println!("  status:    {}", ws.status);
    println!("  attention: {}", attention_summary(&ws));
    println!("  channel:   {}", ws.id);
    if let Some(n) = ws.tracking_issue {
        println!("  track:     Loom issue #{n}");
    }
    println!("  activity:  {}", ws.last_activity_at);
    Ok(())
}

/// `loom sessions wait` — block until the session finishes, is lost, or (unless
/// `lifecycle_only`) its agent raises attention.
pub async fn cmd_session_wait(
    key: String,
    timeout: u64,
    interval: u64,
    lifecycle_only: bool,
) -> Result<()> {
    let client = client::default()?;
    // Short-circuit if the session is already in a wake state at call time.
    let ws = fetch_session(&client, &key).await?;
    if let Some(reason) = wake_reason(&ws, &key, lifecycle_only) {
        println!("{reason}");
        return Ok(());
    }
    println!("waiting on {} ({}) — {}", key, ws.branch.name, ws.status);

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
                ws.status
            );
        }
    }
}

/// Why a `wait` should stop watching `ws`, or `None` to keep waiting: a terminal
/// or orphaned lifecycle, or — unless `lifecycle_only` — a raised attention.
pub fn wake_reason(ws: &SessionView, key: &str, lifecycle_only: bool) -> Option<String> {
    let status = ws.status.as_str();
    if status == "archived" {
        return Some(format!(
            "session {key} is archived — its worktree was torn down (try `loom sessions recover {key}`)"
        ));
    }
    if is_terminal_status(status) {
        return Some(format!("session {key} is {status} — finished"));
    }
    if status == "orphaned" {
        return Some(format!(
            "session {key} is orphaned — its terminal was lost (try `loom sessions adopt {key}`)"
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
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "done" | "error" | "archived")
}

/// `loom sessions send` — type a message into the agent's pane, submitting it
/// (Enter) unless `submit` is false.
pub async fn cmd_session_send(key: String, message: String, submit: bool) -> Result<()> {
    if message.trim().is_empty() {
        bail!("nothing to send — provide a message");
    }
    let client = client::default()?;
    client
        .invoke::<sessions::send::Op>(&sessions::send::Input {
            text: message,
            submit: Some(submit),
            by: None,
            session: key.clone(),
        })
        .await?;
    println!(
        "sent to {key}{}",
        if submit { "" } else { " (not submitted)" }
    );
    Ok(())
}

/// `loom sessions interrupt` — interrupt the agent's current turn.
pub async fn cmd_session_interrupt(key: String) -> Result<()> {
    let client = client::default()?;
    client
        .invoke::<sessions::interrupt::Op>(&sessions::interrupt::Input {
            session: key.clone(),
        })
        .await?;
    println!("interrupted {key}");
    Ok(())
}

/// `loom sessions preview` — print the session's recent terminal screen.
pub async fn cmd_session_preview(key: String, lines: usize) -> Result<()> {
    let client = client::default()?;
    let res = client
        .invoke::<sessions::preview::Op>(&sessions::preview::Input {
            lines: lines as i64,
            session: key,
        })
        .await?;
    print!("{}", res.screen);
    // The capture is right-trimmed server-side; ensure a clean final newline.
    println!();
    Ok(())
}

#[derive(Default)]
pub struct PsOptions {
    archived: bool,
    managed: bool,
    search: Option<String>,
    status: Option<SessionSearchStatus>,
    attention: Option<SessionSearchAttention>,
    creator: Option<SessionCreatorFilter>,
}

pub async fn cmd_ps(options: PsOptions) -> Result<()> {
    let PsOptions {
        archived,
        managed,
        search,
        status,
        attention,
        creator,
    } = options;
    let client = client::default()?;
    let search = search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // `--managed` is the operator inventory: it is the only listing that shows a
    // watcher's own warm sessions, and `sessions.list` refuses it to anything but
    // a human credential. `--status`/`--attention` are excluded from it because
    // the managed survey has never filtered on either, not because it could not.
    if managed && (status.is_some() || attention.is_some()) {
        bail!("--status and --attention cannot be combined with --managed");
    }
    let rows = client
        .invoke::<sessions::list::Op>(&sessions::list::Input {
            q: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .query
            .clone(),
            history: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .history,
            archived_only: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .archived_only,
            status: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .status,
            attention: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .attention,
            creator: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .creator,
            automation: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .automation
            .unwrap_or(true),
            managed: (SearchSessionsOptions {
                query: search.unwrap_or_default().to_string(),
                history: archived,
                archived_only: false,
                status,
                attention,
                creator,
                // The plain fleet listing has always omitted automation
                // sessions; the managed inventory has always included them.
                automation: Some(managed),
                managed,
            })
            .managed,
        })
        .await?;
    if rows.is_empty() {
        let hint = match search {
            Some(s) => format!("no sessions match '{s}'"),
            _ => "no sessions — start one with `loom sessions launch \"<task>\"`".to_string(),
        };
        println!("{hint}");
        return Ok(());
    }
    println!(
        "{:<10}  {:<9}  {:<10}  {:<22}  {:<24}  TITLE",
        "ID", "STATUS", "ATTENTION", "NAME", "LOCATION"
    );
    for ws in &rows {
        // An unplaced session used to print a bare `/`: the untyped reader
        // could not tell `null` from an object with two empty names, so the
        // separator was joining nothing to nothing.
        let location = ws.placement.as_ref().map_or_else(
            || "—".to_string(),
            |placement| format!("{}/{}", placement.space_name, placement.group_name),
        );
        println!(
            "{:<10}  {:<9}  {:<10}  {:<22}  {:<24}  {}",
            ws.id,
            ws.status,
            branch_attention(ws),
            truncate(&ws.branch.name, 22),
            truncate(&location, 24),
            truncate(&ws.branch.title, 46),
        );
    }
    Ok(())
}

pub async fn cmd_show(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::get::Op>(&sessions::get::Input { session: key })
        .await?;
    print_session(&ws);
    Ok(())
}

/// `loom sessions rename` — set a session's one-line dashboard title
/// (`sessions.update`). This keeps the CLI at parity with the dashboard's inline
/// title editor: the observed label and provenance travel with the edit so a
/// concurrent rename is rejected rather than silently overwritten.
pub async fn cmd_session_rename(key: String, title: String) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!("nothing to rename to — provide a new title");
    }
    let client = client::default()?;
    let current = client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: key.clone(),
        })
        .await?;
    let ws = client
        .invoke::<sessions::update::Op>(&sessions::update::Input {
            title: Some(title.to_string()),
            expected_title: Some(current.branch.title),
            expected_title_provenance: Some(current.branch.title_provenance),
            session: key,
            ..Default::default()
        })
        .await?;
    println!("renamed {} → {}", ws.id, ws.branch.title);
    Ok(())
}

pub async fn cmd_session_regenerate_title(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::title::regenerate::Op>(&sessions::title::regenerate::Input {
            session: key,
        })
        .await?;
    println!("{} — {}", ws.branch.title, ws.title_generation.status);
    Ok(())
}

pub async fn cmd_session_title_generation(key: String, enabled: bool) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::title::generation::set::Op>(&sessions::title::generation::set::Input {
            enabled,
            session: key,
        })
        .await?;
    println!(
        "title generation {} ({})",
        if enabled { "enabled" } else { "disabled" },
        ws.title_generation.status
    );
    Ok(())
}

/// How long `session cue --ensure` follows a generation it started. Covers the
/// server's own 45s prompt timeout with room for the runtime to spawn first.
pub const CUE_POLL_ATTEMPTS: usize = 40;

pub const CUE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub async fn cmd_session_cue(key: String, ensure: bool, force: bool) -> Result<()> {
    let client = client::default()?;
    let mut cue = if ensure {
        client
            .invoke::<sessions::resumption_cue::ensure::Op>(
                &sessions::resumption_cue::ensure::Input {
                    force,
                    session: key.clone(),
                },
            )
            .await?
    } else {
        client
            .invoke::<sessions::resumption_cue::get::Op>(&sessions::resumption_cue::get::Input {
                session: key.clone(),
            })
            .await?
    };
    // An ensure only *starts* generation — the model call runs detached so it
    // cannot hold a connection open. Wait it out here so the command still
    // prints a cue rather than the status of a request that just left.
    if ensure {
        for _ in 0..CUE_POLL_ATTEMPTS {
            if cue.status != "generating" {
                break;
            }
            tokio::time::sleep(CUE_POLL_INTERVAL).await;
            cue = client
                .invoke::<sessions::resumption_cue::get::Op>(
                    &sessions::resumption_cue::get::Input {
                        session: key.clone(),
                    },
                )
                .await?;
        }
    }
    println!("status: {}", cue.status);
    if let Some(text) = &cue.text {
        println!("{text}");
    }
    if let Some(at) = &cue.generated_at {
        println!("generated: {at}");
    }
    Ok(())
}

pub fn print_session(ws: &SessionView) {
    println!("session {}  ({})", ws.id, ws.branch.name);
    println!(
        "  title:    {} ({})",
        ws.branch.title, ws.branch.title_provenance
    );
    println!(
        "  title AI: {} ({})",
        if ws.title_generation.enabled {
            "enabled"
        } else {
            "disabled"
        },
        ws.title_generation.status
    );
    println!("  status:   {}", ws.status);
    if let Some(placement) = &ws.placement {
        println!(
            "  location: {} / {}",
            placement.space_name, placement.group_name
        );
    }
    // Agent-declared attention level (the resolved `attention` tag) plus its
    // current-state message (the branch `description`), shown together — one
    // signal.
    println!("  attention: {}", attention_summary(ws));
    let goal = &ws.branch.goal;
    println!(
        "  goal:     {}",
        if goal.is_empty() { "(none)" } else { goal }
    );
    println!("  agent:    {}", ws.agent_kind);
    if !ws.model.is_empty() {
        println!("  model:    {}", ws.model);
    }
    if !ws.effort.is_empty() {
        println!("  effort:   {}", ws.effort);
    }
    println!(
        "  branch:   {} (base {})",
        ws.branch.branch, ws.branch.base_branch
    );
    let exact_parent = ws.parent_session_id.as_deref().unwrap_or_default();
    if !exact_parent.is_empty() {
        println!("  parent:   session {exact_parent}");
    } else {
        let legacy_parent = ws.parent_id.as_deref().unwrap_or_default();
        if !legacy_parent.is_empty() {
            println!("  parent:   branch {legacy_parent} (legacy)");
        }
    }
    println!("  work_dir: {}", ws.work_dir);
    println!("  session:  {}", ws.term_session);
    if let Some(repo) = &ws.github_repo {
        if !repo.is_empty() {
            println!("  github:   {repo}");
        }
    }
    // The branch's PR snapshot, when loom has polled one (see `crate::github`).
    if let Some(gh) = &ws.branch.github {
        let mut bits = vec![gh.pr_state.to_lowercase()];
        if let Some(review) = &gh.review_decision {
            bits.push(review.to_lowercase().replace('_', " "));
        }
        if let Some(checks) = &gh.checks {
            bits.push(format!("checks {checks}"));
        }
        let bits: Vec<String> = bits.into_iter().filter(|b| !b.is_empty()).collect();
        println!(
            "  pr:       #{} {} ({})",
            gh.pr_number,
            gh.pr_url,
            bits.join(", ")
        );
    }
    println!("  activity: {}", ws.last_activity_at);
}

pub async fn cmd_attach(key: String) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::get::Op>(&sessions::get::Input { session: key })
        .await?;
    let session = ws.term_session.as_str();
    // The `tapestry` binary ships beside `loom`; resolve it as a sibling so an
    // attach works regardless of PATH, then hand off to its native attach.
    let tapestry = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|d| d.join("tapestry"))
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "tapestry".to_string());
    let err = std::process::Command::new(tapestry)
        .args(["attach", session])
        .exec();
    Err(anyhow!("failed to exec terminal attach: {err}"))
}

pub async fn cmd_archive(key: String) -> Result<()> {
    let client = client::default()?;
    let res = client
        .invoke::<sessions::archive::Op>(&sessions::archive::Input {
            session: key.clone(),
        })
        .await?;
    if res.kind == "launch_attempt" {
        println!("archived launch attempt {key} (reserved runtime removed; history kept)");
    } else {
        println!(
            "archived {} (terminal + worktree removed; branch and history kept)",
            res.branch
        );
    }
    for w in &res.warnings {
        eprintln!("  warning: {w}");
    }
    Ok(())
}

pub async fn cmd_adopt(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::adopt::Op>(&sessions::adopt::Input { session: key })
        .await?;
    println!("adopted session {}  ({})", ws.id, ws.branch.name);
    println!("  status:  {}", ws.status);
    println!("  session: {}", ws.term_session);
    println!("  attach:  loom attach {}", ws.id);
    Ok(())
}

pub async fn cmd_recover(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .invoke::<sessions::recover::Op>(&sessions::recover::Input { session: key })
        .await?;
    println!("recovered session {}  ({})", ws.id, ws.branch.name);
    println!("  status:  {}", ws.status);
    println!("  session: {}", ws.term_session);
    println!("  attach:  loom attach {}", ws.id);
    Ok(())
}

pub async fn cmd_handoff(
    key: String,
    profile: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<String>,
) -> Result<()> {
    let client = client::default()?;
    let request = if let Some(profile) = profile {
        let selection = weaver_api::LaunchSelection {
            profile,
            overrides: weaver_api::LaunchOverrides {
                agent,
                model,
                effort,
                mode,
                ..Default::default()
            },
        };
        let preview = client
            .invoke::<sessions::handoff::resolve::Op>(&sessions::handoff::resolve::Input {
                selection: selection.clone(),
                session: key.to_string(),
            })
            .await?;
        if !preview.valid {
            bail!(
                "handoff settings are not currently valid:\n{}",
                preview.errors.join("\n")
            );
        }
        weaver_api::HandoffReq {
            selection: Some(selection),
            expected_profile_revision: Some(preview.profile_revision),
            expected_resolver_revision: Some(preview.resolver_revision),
            ..Default::default()
        }
    } else {
        let agent = agent.ok_or_else(|| {
            anyhow::anyhow!("handoff requires either --profile or the legacy --agent selector")
        })?;
        weaver_api::HandoffReq {
            agent,
            model,
            effort,
            mode,
            ..Default::default()
        }
    };
    let ws = client
        .invoke::<sessions::handoff::Op>(&sessions::handoff::Input {
            agent: request.agent.clone(),
            model: request.model.clone(),
            effort: request.effort.clone(),
            mode: request.mode.clone(),
            selection: request.selection.clone(),
            expected_profile_revision: request.expected_profile_revision,
            expected_resolver_revision: request.expected_resolver_revision.clone(),
            session: key.to_string(),
        })
        .await?;
    println!("handed off session {} to {}", ws.id, ws.agent_kind);
    if !ws.model.is_empty() {
        println!("  model:   {}", ws.model);
    }
    if !ws.effort.is_empty() {
        println!("  effort:  {}", ws.effort);
    }
    println!("  session: {}", ws.term_session);
    Ok(())
}

pub async fn cmd_rm(key: String, keep_branch: bool) -> Result<()> {
    let client = client::default()?;
    let res = client
        .invoke::<sessions::delete::Op>(&sessions::delete::Input {
            keep_branch,
            session: key.clone(),
        })
        .await?;
    println!("removed session {key}");
    for w in &res.warnings {
        eprintln!("  warning: {w}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_statuses_match_the_session_model() {
        for s in ["done", "error", "archived"] {
            assert!(is_terminal_status(s), "{s} should be terminal");
        }
        for s in ["created", "running", "orphaned"] {
            assert!(!is_terminal_status(s), "{s} should not be terminal");
        }
    }
    /// Decoding the fixture through `SessionView` keeps it answerable to the
    /// wire contract: a renamed field fails here instead of silently reading as
    /// absent.
    fn view(status: &str, attention: &str, description: &str) -> SessionView {
        // `ok` is the calm, tag-less state; any other level is the `attention`
        // tag's value, mirroring the wire `branch.tags` shape.
        let tags = if attention == "ok" {
            json!([])
        } else {
            json!([{
                "key": "attention",
                "value": attention,
                "note": "",
                "set_by": "agent",
                "set_at": "",
            }])
        };
        serde_json::from_value(json!({
            "id": "s",
            "status": status,
            "work_dir": "",
            "term_session": "",
            "agent_kind": "",
            "model": "",
            "effort": "",
            "github_repo": null,
            "last_activity_at": "",
            "created_at": "",
            "updated_at": "",
            "parent_id": null,
            "created_by": null,
            "tracking_issue": null,
            "park": null,
            "sort_order": null,
            "branch": {
                "id": "",
                "name": "",
                "title": "",
                "goal": "",
                "description": description,
                "tags": tags,
                "repo_root": "",
                "branch": "",
                "base_branch": "",
                "created_at": "",
                "updated_at": "",
                "open_issue_count": 0,
                "github": null,
                "github_pr": null,
            },
        }))
        .expect("fixture must decode as a SessionView")
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
            profile: None,
            name: None,
            agent: None,
            repo: None,
            base: None,
            title: None,
            issue: None,
            claim: None,
            branch: None,
            model: None,
            effort: None,
            protocol: None,
            mode: None,
        }
    }
    // Serial: reads the process's current directory, which the precedence test
    // below moves.
    #[serial_test::serial]
    #[test]
    fn resolve_repo_target_reads_a_local_checkout() {
        // No `--repo` falls back to the current directory.
        let here = std::env::current_dir().unwrap();
        assert_eq!(resolve_repo_target(None).unwrap(), RepoTarget::Local(here));

        // A path that exists is a local checkout, canonicalized.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        assert_eq!(
            resolve_repo_target(Some(&path)).unwrap(),
            RepoTarget::Local(dir.path().canonicalize().unwrap())
        );
    }
    #[test]
    fn resolve_repo_target_reads_a_repo_to_clone() {
        // A repo this machine has never checked out: the whole point — it is
        // handed to the server as a managed repo rather than failing as a path.
        for input in [
            "marin-community/vllm",
            "https://github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
        ] {
            assert_eq!(
                resolve_repo_target(Some(input)).unwrap(),
                RepoTarget::Managed(input.to_string()),
                "input: {input}"
            );
        }
    }
    #[test]
    fn resolve_repo_target_rejects_what_is_neither() {
        // A typo'd path that can't be a repo reference either fails here, not as
        // an opaque server error.
        let dir = tempfile::tempdir().unwrap();
        for bad in [
            dir.path().join("nope").to_string_lossy().to_string(),
            "../not-a-checkout".to_string(),
            "one-segment".to_string(),
        ] {
            assert!(resolve_repo_target(Some(&bad)).is_err(), "bad: {bad}");
        }
    }
    /// A real directory in front of you is never a guess: `acme/widgets` is a
    /// perfectly good slug, but when it also *exists* as a relative path it stays
    /// local rather than being hijacked into a clone of the GitHub repo that
    /// happens to share its spelling. Only a relative path can collide with a
    /// slug like this, so the test has to work from a real cwd (hence `serial` —
    /// it moves the process's current directory).
    #[serial_test::serial]
    #[test]
    fn resolve_repo_target_prefers_an_existing_path_over_a_slug() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("acme").join("widgets");
        std::fs::create_dir_all(&nested).unwrap();

        let restore = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let resolved = resolve_repo_target(Some("acme/widgets"));
        std::env::set_current_dir(restore).unwrap();

        // Same spelling as the slug — and it resolves to the directory, not a clone.
        assert_eq!(
            resolved.unwrap(),
            RepoTarget::Local(nested.canonicalize().unwrap())
        );
        // With no such directory around, the very same string is a repo to clone.
        assert_eq!(
            resolve_repo_target(Some("acme/widgets")).unwrap(),
            RepoTarget::Managed("acme/widgets".to_string())
        );
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
}
