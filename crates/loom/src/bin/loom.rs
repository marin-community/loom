//! loom — the orchestration CLI.
//!
//! Most subcommands talk to the running loom daemon over HTTP (session
//! lifecycle, archive, adopt). `loom server run` runs the daemon itself in the
//! foreground; `loom server start`/`stop`/`restart`/`status` manage its
//! background lifecycle. To interact with an agent, `loom session attach` to its
//! terminal (the browser terminal is the other interaction surface).

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use loom::client::{self, Client};
use weaver_core::db::Db;

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
    /// Manage the loom server daemon: run, start, stop, restart, status.
    ///
    /// `loom server run` runs the server in the foreground (REST API + Vue UI +
    /// monitor loop), blocking until interrupted — the form to run under a
    /// process supervisor (systemd, Docker) or while developing. `loom server
    /// start` runs that same process in the background and waits for it to come
    /// up.
    Server {
        #[command(subcommand)]
        cmd: ServerCmd,
    },

    /// Manage sessions: launch, ls, attach, poll, wait, send, break, preview.
    ///
    /// The uniform surface for a child session — start one, list the fleet,
    /// watch one, and interact with its agent pane:
    ///
    ///     loom session launch "Add a /health endpoint and a test for it"
    ///     loom session ls                      # the active fleet
    ///     loom session poll weaver/health      # one-shot status
    ///     loom session wait weaver/health      # block until done / needs you
    ///     loom session send weaver/health "try the curl again"
    ///     loom session break weaver/health     # interrupt the current turn
    ///     loom session preview weaver/health   # peek at the terminal screen
    ///     loom session rename weaver/health "Health endpoint + test"
    ///
    /// The three you reach for constantly have top-level shortcuts: `loom
    /// launch`, `loom ps`, `loom attach`.
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
    /// Manage API tokens for automation (the `LOOM_TOKEN` a CI job presents).
    ///
    /// Mint a token to drive loom from GitHub Actions or any remote client:
    ///
    ///     loom token add github-actions        # prints the secret once — copy it now
    ///     loom token ls                         # name, prefix, last used
    ///     loom token rm <id>                    # revoke
    ///
    /// Store the printed secret as a CI secret and pass it as `LOOM_TOKEN` (with
    /// `WEAVER_API` pointing at your server) — every `loom` command then
    /// authenticates with it.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Show the repo's issue board (every issue across branches + backlog).
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },

    /// Guided one-time credential setup.
    ///
    /// `loom setup` with no subcommand runs the **interactive walkthrough**: it
    /// establishes a bootstrap operator (so the daemon can start and someone can
    /// sign in), then optionally the GitHub App and the agent secrets — one
    /// command to get a fresh instance ready. Re-running it is safe: each step
    /// pre-fills its default from the existing config, so it updates in place
    /// rather than starting over. The subcommands below run an individual step
    /// directly.
    ///
    /// `loom setup github-app` registers the GitHub App loom uses (the
    /// webhook receiver + REST identity from `docs/github-trigger.md`, which
    /// doubles as the "Sign in with GitHub" app) via GitHub's **manifest
    /// flow**: it opens a local page that auto-submits to GitHub, you confirm
    /// once, and loom exchanges the redirect for the full credential set —
    /// app id, private key, webhook secret, OAuth client — writing them
    /// straight into loom's settings. No `.env` editing, no restart. When an App
    /// is already configured it instead offers to update its permissions or
    /// re-install it (opening the right GitHub page), or to replace it.
    ///
    ///     loom setup github-app --base-url https://loom.team.dev
    ///
    /// `loom setup secrets` prompts for the paste-once secrets every agent
    /// session needs (an Anthropic API key, a GitHub token) and stores them as
    /// operator environment variables, live for every session launched from
    /// then on:
    ///
    ///     loom setup secrets
    Setup {
        /// A specific step to run directly. Omit it to run the interactive
        /// walkthrough (which always establishes a bootstrap operator first).
        #[command(subcommand)]
        cmd: Option<SetupCmd>,
    },

    /// The typed `loom.toml` `loom setup` writes and everything derived from
    /// it, plus `set` — a direct-to-sqlite write of the daemon's own runtime
    /// `settings` table (the same keys `weaver config set` exposes over
    /// HTTP), with no server required.
    ///
    /// `loom.toml` is the single authored source of truth for every
    /// credential/setting — the shared contract a deploy (e.g. the GCP
    /// scripts under `deploy/gcp`) builds against:
    ///
    ///     loom config render-env                # -> deploy/standalone/.env
    ///     loom config secret-names               # the secret fields' ENV_NAMEs
    ///     loom config push-secrets --backend gcp --project my-project
    ///     loom config set auth.cookie_secure true  # direct-to-sqlite, no server needed
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },

    /// Launch a new session — shortcut for `loom session launch`.
    Launch(LaunchOpts),
    /// List active sessions — shortcut for `loom session ls`.
    Ps,
    /// Attach your terminal to a session — shortcut for `loom session attach`.
    Attach { branch: String },

    /// Open the loom web UI in a browser.
    Open,
    /// Generate shell completions.
    Completions { shell: clap_complete::Shell },
}

/// Subcommands under `loom server` — the daemon lifecycle.
#[derive(Subcommand)]
enum ServerCmd {
    /// Run the server in the foreground (REST API + Vue UI + monitor loop).
    ///
    /// Blocks until interrupted — the form to run under a process supervisor
    /// (systemd, Docker) or while developing/testing. `loom server start` runs
    /// this same process in the background.
    Run {
        #[arg(long)]
        addr: Option<String>,
    },
    /// Start the server in the background (daemonize) and wait for it to be healthy.
    Start,
    /// Stop the background server.
    Stop,
    /// Stop and re-start the background server.
    Restart,
    /// Show the running server's status.
    Status,
}

/// Subcommands under `loom issue` — the read-only issue board.
#[derive(Subcommand)]
enum IssueCmd {
    /// Show the repo's issue board (every issue across branches + backlog).
    Ls {
        /// Include closed issues.
        #[arg(long)]
        all: bool,
        /// Show only the unclaimed backlog.
        #[arg(long)]
        backlog: bool,
    },
}

/// Subcommands under `loom session` — the uniform way to drive a child session.
#[derive(Subcommand)]
enum SessionCmd {
    /// Launch a new session: worktree + terminal + agent, seeded with a task.
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
    /// Print a session's recent terminal screen.
    Preview {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// Extra scrollback lines above the visible screen (0 = visible only).
        #[arg(long, default_value = "0")]
        lines: usize,
    },
    /// List active sessions (also `loom ps`).
    ///
    /// Archived (torn-down) sessions are hidden by default — pass `--archived`
    /// to include them. `--search <text>` narrows to sessions whose title,
    /// branch name, or goal contains the text. The list is an index: it shows
    /// each session's id, lifecycle, attention, and title — pull the full detail
    /// (goal, PR, dirs, activity) for one with `loom session show <id>`.
    Ls {
        /// Include archived (torn-down) sessions.
        #[arg(long)]
        archived: bool,
        /// Case-insensitive substring filter over title / branch / goal.
        #[arg(long)]
        search: Option<String>,
    },
    /// Rename a session: set the one-line title shown on the dashboard.
    Rename {
        /// Session key: id, branch id, branch name, or `repo:branch`.
        session: String,
        /// The new title. Multiple words are joined, so quoting is optional.
        title: Vec<String>,
    },
    /// Show one session's details.
    Show { branch: String },
    /// Attach your terminal to a session (also `loom attach`).
    Attach { branch: String },
    /// Archive a session: tear down terminal + worktree, keep branch + history.
    Archive { branch: String },
    /// Recreate the terminal session for an orphaned session.
    Adopt { branch: String },
    /// Remove a session (worktree + terminal + DB row).
    Rm {
        branch: String,
        #[arg(long)]
        keep_branch: bool,
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

/// Subcommands under `loom setup` — the guided credential wizards.
#[derive(Subcommand)]
enum SetupCmd {
    /// Create the GitHub App loom uses, via GitHub's manifest flow.
    GithubApp(GithubAppOpts),
    /// Prompt for and store the paste-once agent secrets (Anthropic key, GitHub token).
    Secrets(SecretsOpts),
}

#[derive(Args)]
struct GithubAppOpts {
    /// loom's public base URL, e.g. `https://loom.team.dev` (`localhost:7878`
    /// for a local try-out). Becomes the App's homepage, webhook target
    /// (`{base_url}/api/github/webhook`), and OAuth-login callback base
    /// (`{base_url}/api/auth/github/callback`).
    #[arg(long)]
    base_url: String,
    /// The App's display name — must be unique across all of GitHub. Defaults
    /// to `loom-<host>`, derived from `--base-url`.
    #[arg(long)]
    name: Option<String>,
    /// Create the App under this GitHub organization instead of your personal
    /// account.
    #[arg(long)]
    org: Option<String>,
    /// The GitHub login approved to sign in first (`LOOM_OWNER_GITHUB`).
    /// Required with `--org`: an org install's App is owned by the org, but
    /// the first approved sign-in needs an individual login, which the org's
    /// own login isn't — prompted for interactively if omitted. Optional
    /// without `--org`, where it defaults to your own account (the one that
    /// confirms App creation).
    #[arg(long)]
    owner: Option<String>,
    /// Local port for the manifest-flow confirmation callback. `0` (default)
    /// picks a free port; pin one when you're tunnelling in to a remote host
    /// (e.g. `ssh -L 8765:localhost:8765 …`, then `--port 8765`).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// How long to wait for the browser confirmation, in seconds.
    #[arg(long, default_value = "300")]
    timeout: u64,
    /// Don't try to open a browser automatically — just print the confirmation
    /// page's URL.
    #[arg(long)]
    no_open: bool,
    #[command(flatten)]
    config: ConfigPathOpts,
}

#[derive(Args)]
struct SecretsOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
}

/// Shared `--config` flag: the authored `loom.toml`, the single source of
/// truth every `loom setup` wizard fills in and `loom config` reads from.
#[derive(Args)]
struct ConfigPathOpts {
    /// Path to `loom.toml`. Defaults to `./loom.toml`, or `$LOOM_CONFIG`.
    #[arg(long, env = loom::loom_config::CONFIG_ENV_VAR, default_value = loom::loom_config::DEFAULT_PATH)]
    config: std::path::PathBuf,
}

/// Subcommands under `loom config` — the typed `loom.toml` and everything
/// rendered/pushed from it. The contract a deploy (e.g. `deploy/gcp`) builds
/// against — see [`Cmd::Config`]. `render-env` and `push-secrets` resolve
/// every field from `loom.toml` *or* a same-named env var (env wins) — set
/// one to override a single invocation without editing the file.
///
/// `set` is a different contract — the runtime `settings` table
/// (`weaver_core::config::REGISTRY`, the same one `weaver config set` writes
/// over HTTP) rather than `loom.toml` — written straight to the daemon's
/// sqlite database with no server needed. This is what
/// `deploy/standalone/docker-compose.yml`'s `loom-init` uses to seed the
/// security-relevant auth settings before loom itself starts listening.
#[derive(Subcommand)]
enum ConfigCmd {
    /// Render `loom.toml` as a dotenv file (e.g. `deploy/standalone/.env`).
    RenderEnv(RenderEnvOpts),
    /// Print each secret field's `ENV_NAME`, one per line.
    SecretNames(ConfigPathOpts),
    /// Push each secret field's value to a secret-manager backend. Never
    /// echoes a value.
    PushSecrets(PushSecretsOpts),
    /// Set a runtime setting directly in the sqlite `settings` table — no
    /// running server needed (unlike `weaver config set`, which needs one).
    Set {
        /// Dotted key, e.g. `auth.cookie_secure` (see the settings pane, or
        /// `weaver_core::config::REGISTRY`, for the full list).
        key: String,
        value: String,
    },
}

#[derive(Args)]
struct RenderEnvOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
    /// Where to write the rendered dotenv file. `-` writes to stdout instead.
    #[arg(long, default_value = "deploy/standalone/.env")]
    out: String,
}

#[derive(Args)]
struct PushSecretsOpts {
    #[command(flatten)]
    config: ConfigPathOpts,
    /// Secret-manager backend to push to.
    #[arg(long, value_enum)]
    backend: SecretBackend,
    /// The GCP project id to push into.
    #[arg(long)]
    project: String,
}

#[derive(Clone, clap::ValueEnum)]
enum SecretBackend {
    Gcp,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Mint a new API token. Prints the secret once — copy it now.
    Add {
        /// A label to recognise the token by (e.g. `github-actions`).
        name: String,
        /// Optional lifetime in days; omit for a non-expiring token.
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// List the API tokens (name, prefix, created, last used).
    Ls,
    /// Revoke a token by id.
    Rm {
        /// The token id (from `loom token ls`).
        id: String,
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
/// top-level `loom launch` shortcut.
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
    /// Repo to launch into: a path to (any directory inside) the target repo's
    /// checkout. The new worktree is cut from this repo's mainline. Defaults to
    /// the current directory — so without it you launch into whatever repo you
    /// happen to be standing in, which is the wrong one when you mean another.
    #[arg(long)]
    repo: Option<std::path::PathBuf>,
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
    /// Model selector accepted by the selected agent. Omit to use the selected
    /// agent's default.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort: low, medium, high, xhigh, or max. Omit to use the
    /// selected agent's default.
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
        Cmd::Server { cmd } => run_server(cmd).await,
        Cmd::Session { cmd } => run_session(cmd).await,
        Cmd::Issue { cmd } => run_issue(cmd).await,
        Cmd::Overlooker { cmd } => run_overlooker(cmd).await,
        Cmd::Token { cmd } => run_token(cmd).await,
        Cmd::Setup { cmd } => run_setup(cmd).await,
        Cmd::Config { cmd } => run_config(cmd).await,
        Cmd::Launch(opts) => cmd_launch(opts.into()).await,
        Cmd::Ps => cmd_ps(false, None).await,
        Cmd::Attach { branch } => cmd_attach(branch).await,
        Cmd::Open => cmd_open().await,
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "loom", &mut std::io::stdout());
            Ok(())
        }
    }
}

/// Dispatch the `loom server <verb>` daemon-lifecycle subcommands.
async fn run_server(cmd: ServerCmd) -> Result<()> {
    match cmd {
        ServerCmd::Run { addr } => {
            init_tracing();
            let addr = loom::endpoint::bind_addr(addr.as_deref());
            loom::server::run(&addr).await
        }
        ServerCmd::Start => cmd_start().await,
        ServerCmd::Stop => cmd_stop().await,
        ServerCmd::Restart => cmd_restart().await,
        ServerCmd::Status => cmd_status().await,
    }
}

/// Dispatch the `loom issue <verb>` subcommands (the read-only board).
async fn run_issue(cmd: IssueCmd) -> Result<()> {
    match cmd {
        IssueCmd::Ls { all, backlog } => cmd_issues(all, backlog).await,
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
        SessionCmd::Ls { archived, search } => cmd_ps(archived, search).await,
        SessionCmd::Rename { session, title } => cmd_session_rename(session, title.join(" ")).await,
        SessionCmd::Show { branch } => cmd_show(branch).await,
        SessionCmd::Attach { branch } => cmd_attach(branch).await,
        SessionCmd::Archive { branch } => cmd_archive(branch).await,
        SessionCmd::Adopt { branch } => cmd_adopt(branch).await,
        SessionCmd::Rm {
            branch,
            keep_branch,
        } => cmd_rm(branch, keep_branch).await,
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

async fn run_token(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::Add { name, expires_days } => cmd_token_create(name, expires_days).await,
        TokenCmd::Ls => cmd_token_ls().await,
        TokenCmd::Rm { id } => cmd_token_rm(id).await,
    }
}

async fn cmd_token_create(name: String, expires_days: Option<i64>) -> Result<()> {
    let created = client::default()
        .create_token(&weaver_api::CreateTokenReq {
            name,
            expires_in_days: expires_days,
        })
        .await?;
    // The secret is shown once; lead with it and make the one-shot nature plain.
    println!("{}", created.token);
    eprintln!(
        "\nThis is the only time the token is shown. Store it now, e.g. as a CI \
         secret, and pass it as LOOM_TOKEN.\nid {}  ·  {}{}",
        created.info.id,
        created.info.prefix,
        match created.info.expires_at {
            Some(at) => format!("  ·  expires {at}"),
            None => "  ·  never expires".to_string(),
        }
    );
    Ok(())
}

async fn cmd_token_ls() -> Result<()> {
    let tokens = client::default().list_tokens().await?;
    if tokens.is_empty() {
        println!("no tokens — create one with `loom token add <name>`");
        return Ok(());
    }
    println!("{:<18}  {:<20}  {:<16}  LAST USED", "ID", "NAME", "PREFIX");
    for t in tokens {
        println!(
            "{:<18}  {:<20}  {:<16}  {}",
            t.id,
            truncate(&t.name, 20),
            t.prefix,
            t.last_used_at.as_deref().unwrap_or("never"),
        );
    }
    Ok(())
}

async fn cmd_token_rm(id: String) -> Result<()> {
    client::default().revoke_token(&id).await?;
    println!("revoked token {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Setup wizards (github-app, secrets)
// ---------------------------------------------------------------------------

async fn run_setup(cmd: Option<SetupCmd>) -> Result<()> {
    match cmd {
        None => cmd_setup_init().await,
        Some(SetupCmd::GithubApp(opts)) => cmd_setup_github_app(opts).await,
        Some(SetupCmd::Secrets(opts)) => cmd_setup_secrets(opts).await,
    }
}

/// The default `loom.toml` path, mirroring [`ConfigPathOpts`]'s clap resolution
/// (`$LOOM_CONFIG`, else `./loom.toml`) for the walkthrough, which takes no flag.
fn default_config_path() -> std::path::PathBuf {
    std::env::var(loom::loom_config::CONFIG_ENV_VAR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(loom::loom_config::DEFAULT_PATH))
}

/// `loom setup` with no subcommand — the guided walkthrough. Its one hard
/// guarantee is a **bootstrap operator**: it always seeds one (live into the DB
/// and into `loom.toml`), so the instance can start and someone can sign in —
/// the interactive complement to [`crate::server::ensure_bootstrap_operator`]'s
/// boot guard. The GitHub App and agent-secret steps are offered but skippable,
/// and delegate to the same [`cmd_setup_github_app`]/[`cmd_setup_secrets`] the
/// subcommands use. A failure in an optional step is reported and the walkthrough
/// continues, so a browser timeout can't cost you the operator you just set up.
async fn cmd_setup_init() -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!(
            "loom setup needs an interactive terminal — run it directly (not piped or in CI). \
             For a non-interactive deploy, set LOOM_OWNER_GITHUB (and the other LOOM_* vars) \
             and run `loom config render-env` instead."
        );
    }
    let config_path = default_config_path();
    println!(
        "loom setup — I'll ask a few questions, write them to {}, and apply them to the",
        config_path.display()
    );
    println!("database so they take effect immediately.");
    println!();

    let db = loom::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;

    // Pre-fill each step's default from any existing config, so re-running the
    // wizard updates in place instead of restarting from scratch. The operator
    // login falls back to the seeded primary user when loom.toml has none yet.
    let existing_cfg = loom::loom_config::load(&config_path).ok();
    let prefill_owner = existing_cfg
        .as_ref()
        .and_then(|c| c.owner_github.clone())
        .or(loom::auth::primary_user(&db).await.ok().flatten());
    let prefill_base_url = existing_cfg
        .as_ref()
        .and_then(|c| c.domain.as_deref())
        .and_then(base_url_from_domain);

    // Step 1 — bootstrap operator (required). Without one, no one can sign in
    // and the daemon refuses to start, so this step cannot be skipped.
    println!("Step 1/4 · Bootstrap operator (required)");
    println!("  The GitHub login allowed to sign in first and approve everyone else.");
    let owner = loop {
        let login = prompt_line("GitHub login", prefill_owner.as_deref())?;
        if loom::owners::valid_login(&login) {
            break login;
        }
        println!("  '{login}' isn't a valid GitHub login (letters, digits, and hyphens only).");
    };
    if loom::auth::get_user(&db, &owner).await?.is_none() {
        loom::auth::add_user(&db, &owner, Some(&owner), None)
            .await
            .with_context(|| format!("seeding the bootstrap operator '{owner}'"))?;
    }
    loom::owners::add(&db, &owner)
        .await
        .context("trusting the bootstrap owner")?;
    // Merge (don't overwrite) the allowlist, so re-running the walkthrough never
    // drops owners already in loom.toml.
    let allowed_owners = merged_allowed_owners(&config_path, &owner);
    loom::loom_config::upsert(
        &config_path,
        &[
            ("LOOM_OWNER_GITHUB", owner.as_str()),
            ("LOOM_ALLOWED_OWNERS", allowed_owners.as_str()),
        ],
    )
    .context("writing the operator into loom.toml")?;
    println!("  ✓ '{owner}' can sign in and is a trusted owner.");
    println!();

    // Step 2 — public URL.
    println!("Step 2/4 · Public URL");
    println!("  Where loom is reachable; localhost for a local try-out.");
    let base_url = prompt_line(
        "Base URL",
        prefill_base_url
            .as_deref()
            .or(Some("http://localhost:7878")),
    )?
    .trim_end_matches('/')
    .to_string();
    let domain = host_from_base_url(&base_url).to_string();
    loom::loom_config::upsert(&config_path, &[("LOOM_DOMAIN", domain.as_str())])
        .context("writing the domain into loom.toml")?;
    println!();

    // Step 3 — GitHub App (optional; opens a browser). Delegates to the same
    // wizard the subcommand uses, passing the operator so it stays consistent.
    println!("Step 3/4 · GitHub App (recommended — opens your browser)");
    // An App already on file turns this step into an update/re-install (the
    // create-vs-update choice itself is offered inside `cmd_setup_github_app`).
    let app_exists = existing_app(&db).await.is_some();
    if app_exists {
        println!("  A GitHub App is already configured — you can update or re-install it.");
    } else {
        println!("  Creates the App loom acts through (webhook, sign-in, per-repo tokens).");
    }
    let step3_prompt = if app_exists {
        "Review / update the GitHub App now?"
    } else {
        "Set up the GitHub App now?"
    };
    if prompt_yes_no(step3_prompt, true)? {
        let app_opts = GithubAppOpts {
            base_url: base_url.clone(),
            name: None,
            org: None,
            owner: Some(owner.clone()),
            port: 0,
            timeout: 300,
            no_open: false,
            config: ConfigPathOpts {
                config: config_path.clone(),
            },
        };
        if let Err(e) = cmd_setup_github_app(app_opts).await {
            println!("  ! GitHub App setup didn't complete: {e}");
            println!("  Retry later with `loom setup github-app --base-url {base_url}`.");
        }
    } else if app_exists {
        println!("  Left the existing App as-is.");
    } else {
        println!("  Skipped — set it up later with `loom setup github-app`.");
    }
    println!();

    // Step 4 — agent secrets. Same wizard the subcommand uses.
    println!("Step 4/4 · Agent secrets");
    if let Err(e) = cmd_setup_secrets(SecretsOpts {
        config: ConfigPathOpts {
            config: config_path.clone(),
        },
    })
    .await
    {
        println!("  ! Secrets step didn't complete: {e}");
        println!("  Retry later with `loom setup secrets`.");
    }
    println!();

    println!("Setup complete. Next: run `loom config render-env` to produce a deploy `.env`,");
    println!("then start the daemon (e.g. `docker compose up -d`).");
    Ok(())
}

/// Prompt (plain text) for one line, showing `default` in brackets and returning
/// it on a blank answer. A `None` default makes the answer required — it
/// re-prompts until non-empty.
fn prompt_line(label: &str, default: Option<&str>) -> Result<String> {
    use std::io::Write;
    loop {
        match default {
            Some(d) => print!("  {label} [{d}]: "),
            None => print!("  {label}: "),
        }
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .with_context(|| format!("reading {label}"))?;
        let value = input.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        println!("  (required)");
    }
}

/// Prompt yes/no, returning `true` for yes; a blank answer takes `default_yes`.
fn prompt_yes_no(label: &str, default_yes: bool) -> Result<bool> {
    use std::io::Write;
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("  {label} {hint}: ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading a yes/no answer")?;
    Ok(match input.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Prompt for one of `options` by number, returning the chosen 0-based index.
/// A blank answer takes `default` (also 0-based); an out-of-range answer
/// re-prompts.
fn prompt_choice(prompt: &str, options: &[&str], default: usize) -> Result<usize> {
    use std::io::Write;
    println!("  {prompt}");
    for (i, opt) in options.iter().enumerate() {
        let marker = if i == default { "  (default)" } else { "" };
        println!("    {}) {opt}{marker}", i + 1);
    }
    loop {
        print!("  Choice [{}]: ", default + 1);
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("reading a menu choice")?;
        let s = input.trim();
        if s.is_empty() {
            return Ok(default);
        }
        match s.parse::<usize>() {
            Ok(n) if (1..=options.len()).contains(&n) => return Ok(n - 1),
            _ => println!("  Enter a number from 1 to {}.", options.len()),
        }
    }
}

/// Open `url` in the operator's browser (best-effort via `xdg-open`), always
/// printing it first so a headless or SSH-tunnelled run can open it by hand.
fn open_browser(url: &str, intro: &str) {
    println!("{intro}");
    println!("  {url}");
    let _ = std::process::Command::new("xdg-open").arg(url).status();
}

/// The GitHub App already recorded in loom's settings, if any. `slug`/`org` may
/// be absent for an App created before setup began recording them
/// ([`loom::github_app::APP_SLUG_KEY`]).
struct ExistingApp {
    id: String,
    slug: Option<String>,
    org: Option<String>,
}

/// Read the configured App from the settings table. `None` when no App id is
/// stored (a fresh instance, or one that only ever used the ambient `GH_TOKEN`).
async fn existing_app(db: &Db) -> Option<ExistingApp> {
    let nonempty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let id = nonempty(loom::config::get(db, loom::github_app::APP_ID_KEY).await)?;
    Some(ExistingApp {
        id,
        slug: nonempty(loom::config::get(db, loom::github_app::APP_SLUG_KEY).await),
        org: nonempty(loom::config::get(db, loom::github_app::APP_OWNER_KEY).await),
    })
}

/// Present the update / re-install menu for an already-configured App. Returns
/// `true` when the operator chose to create a brand-new App (the caller should
/// fall through to the manifest flow, replacing the old one); `false` when the
/// existing App was handled here (a page opened, or left untouched).
async fn offer_existing_app(app: &ExistingApp) -> Result<bool> {
    println!(
        "  A GitHub App is already configured (id {}{}).",
        app.id,
        app.slug
            .as_deref()
            .map(|s| format!(", {s}"))
            .unwrap_or_default()
    );
    let choice = prompt_choice(
        "What would you like to do?",
        &[
            "Update its permissions/settings on GitHub (opens the App's settings page)",
            "Install or re-install it on repositories (opens the install page)",
            "Create a new App to replace it",
            "Leave it as-is",
        ],
        3,
    )?;
    match choice {
        // Update permissions/settings. loom can't change GitHub App permissions
        // itself — the owner edits them in the UI, then each installation
        // re-approves — so this deep-links to the right page and says what to do.
        0 => match &app.slug {
            Some(slug) => {
                let url = loom::github_manifest::settings_url(slug, app.org.as_deref());
                open_browser(
                    &url,
                    "  Opening the App's settings. If nothing opens, visit:",
                );
                println!(
                    "  Adjust Repository permissions as needed (e.g. Pull requests: Read & \
                     write), Save, then accept the updated permissions on each installation."
                );
            }
            None => println!(
                "  This App's slug isn't on record (it predates slug capture). Open your App \
                 settings manually at https://github.com/settings/apps and edit it there."
            ),
        },
        // Install / re-install / adjust repo access. The install page also
        // surfaces any pending permission re-approval.
        1 => match &app.slug {
            Some(slug) => {
                let url = loom::github_manifest::install_url(slug);
                open_browser(&url, "  Opening the install page. If nothing opens, visit:");
            }
            None => println!(
                "  This App's slug isn't on record; find it at \
                 https://github.com/settings/apps and use its Install button."
            ),
        },
        2 => return Ok(true),
        _ => println!("  Left the existing App unchanged."),
    }
    Ok(false)
}

/// Reconstruct a base URL from a stored `LOOM_DOMAIN` for pre-filling the
/// wizard: `localhost` (with no port on record) maps back to the local default,
/// any real domain to `https://<domain>`. `None` for an empty domain.
fn base_url_from_domain(domain: &str) -> Option<String> {
    let d = domain.trim();
    if d.is_empty() {
        None
    } else if d == "localhost" || d.starts_with("localhost:") || d.starts_with("127.0.0.1") {
        Some("http://localhost:7878".to_string())
    } else {
        Some(format!("https://{d}"))
    }
}

/// `loom setup github-app` — the manifest-flow wizard. Talks to GitHub and to
/// loom's sqlite database directly (the same daemon-less path `weaver` uses);
/// it does not need the loom daemon to be running.
async fn cmd_setup_github_app(opts: GithubAppOpts) -> Result<()> {
    let base_url = opts.base_url.trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        bail!("--base-url must be a full URL, e.g. https://loom.team.dev (got '{base_url}')");
    }
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| default_app_name(&base_url));

    let db = loom::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;

    // If an App is already configured, offer to update / re-install it rather
    // than silently registering a second one. Only fall through to the manifest
    // create flow when the operator explicitly chooses to replace it (or when
    // running non-interactively, preserving the historical create behavior).
    if let Some(app) = existing_app(&db).await {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            if !offer_existing_app(&app).await? {
                return Ok(());
            }
            println!();
            println!("Creating a new App to replace the existing one…");
        } else {
            eprintln!(
                "note: a GitHub App (id {}) is already configured; creating another.",
                app.id
            );
        }
    }

    // Which account owns the App: an explicit `--org`, else ask interactively
    // (defaulting to the personal account). A non-interactive run with no `--org`
    // stays personal, preserving the historical default for scripted setups.
    let org: Option<String> = match &opts.org {
        Some(o) => Some(o.clone()),
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                prompt_org()?
            } else {
                None
            }
        }
    };

    // An org-owned App needs an explicit individual owner: the manifest flow's
    // own confirming account (`conv.owner.login`, used below when this is `None`)
    // is the org itself for an org install, which isn't a usable
    // `LOOM_OWNER_GITHUB` — a fresh database with no owner seeded locks everyone
    // out (see `db::seed_owner`). Resolved before opening the callback listener
    // so a misconfigured run fails fast rather than after the operator has
    // already gone through the browser confirmation.
    let org_owner: Option<String> = match (
        &org,
        opts.owner
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(_), Some(o)) => Some(o.to_string()),
        (Some(org), None) => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                Some(prompt_owner(org)?)
            } else {
                bail!(
                    "--org {org} needs --owner <your-github-login> — an org install's App is \
                     owned by the org, but the first approved sign-in needs an individual \
                     login, which the org's own login isn't"
                );
            }
        }
        (None, owner) => owner.map(str::to_string),
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port))
        .await
        .with_context(|| format!("binding the local callback server on port {}", opts.port))?;
    let port = listener
        .local_addr()
        .context("reading the callback server's bound port")?
        .port();
    let redirect_url = format!("http://127.0.0.1:{port}/callback");

    let manifest = loom::github_manifest::manifest_json(&loom::github_manifest::ManifestInput {
        name: &name,
        base_url: &base_url,
        redirect_url: &redirect_url,
    });
    let state = loom::auth::random_state();
    let target = loom::github_manifest::create_url(org.as_deref(), &state);
    let html = loom::github_manifest::submission_html(&manifest, &target);
    // Served at `/` by the same listener that catches the `/callback` redirect
    // — no `file://` URL, so this works unmodified through an SSH tunnel or a
    // one-shot published Docker port when loom isn't running on the machine
    // whose browser you're using.
    let start_url = format!("http://127.0.0.1:{port}/");

    println!("App name:  {name}");
    println!(
        "Owner:     {}",
        org.as_deref().unwrap_or("your personal account")
    );
    println!("Webhook:   {base_url}/api/github/webhook");
    println!("Login:     {base_url}/api/auth/github/callback");
    println!();
    if opts.no_open {
        println!("Open this in a browser to confirm App creation:");
    } else {
        println!("Opening a browser to confirm App creation. If nothing opens, visit:");
        let _ = std::process::Command::new("xdg-open")
            .arg(&start_url)
            .status();
    }
    println!("  {start_url}");
    println!(
        "(Tunnelling from another machine? `ssh -L {port}:localhost:{port} …` then open the \
         same URL there.)"
    );
    println!();
    println!(
        "Waiting for the GitHub confirmation (timeout {}s)…",
        opts.timeout
    );
    let code = loom::github_manifest::run_local_server(
        listener,
        html,
        state,
        std::time::Duration::from_secs(opts.timeout),
    )
    .await?;

    println!("Exchanging the confirmation for credentials…");
    let conv = loom::github_manifest::convert(&code)
        .await
        .context("converting the manifest code into App credentials")?;

    println!();
    println!(
        "Created {} (id {}) under {}",
        conv.slug, conv.id, conv.owner.login
    );
    println!("  {}", conv.html_url);

    loom::config::apply(
        &db,
        &[
            (
                loom::github_app::APP_ID_KEY.to_string(),
                Some(conv.id.to_string()),
            ),
            (
                loom::github_app::APP_PRIVATE_KEY_KEY.to_string(),
                Some(conv.pem.clone()),
            ),
            (
                loom::github_trigger::WEBHOOK_SECRET_KEY.to_string(),
                Some(conv.webhook_secret.clone()),
            ),
            (
                loom::auth::GH_CLIENT_ID_KEY.to_string(),
                Some(conv.client_id.clone()),
            ),
            (
                loom::auth::GH_CLIENT_SECRET_KEY.to_string(),
                Some(conv.client_secret.clone()),
            ),
            // Recorded (not runtime credentials) so a later `loom setup` can
            // deep-link to this App's GitHub settings/install pages to update it.
            (
                loom::github_app::APP_SLUG_KEY.to_string(),
                Some(conv.slug.clone()),
            ),
            (
                loom::github_app::APP_OWNER_KEY.to_string(),
                Some(org.clone().unwrap_or_default()),
            ),
        ],
    )
    .await
    .context("writing the App credentials into loom's settings")?;
    println!();
    println!(
        "Stored the App id, private key, webhook secret, and OAuth client into loom's \
         settings — live on the running daemon, no restart needed."
    );

    let domain = host_from_base_url(&base_url);
    let app_id = conv.id.to_string();
    // The individual who can sign in first (`LOOM_OWNER_GITHUB`): for a personal
    // install the confirming account (`conv.owner.login`); for an org install
    // `org_owner` (the org itself can't sign in, so an individual is required).
    let owner_login = org_owner.as_deref().unwrap_or(conv.owner.login.as_str());
    // The account whose App installations the inbound trigger honors: for an org
    // install that's the *org* (its repos are org-owned, so trusting the
    // individual wouldn't cover them); for a personal install it's the same
    // personal account that signs in.
    let trusted_owner = org.as_deref().unwrap_or(owner_login);

    // Trust that account, so its App installations are honored by the inbound
    // trigger. This is what keeps a *public* App safe: only owners on this
    // allowlist are auto-trusted, never a stranger who installs the App on their
    // own repo. Written live to the running daemon here, and to loom.toml
    // (`LOOM_ALLOWED_OWNERS`) below for a fresh DB. Add more in the UI later.
    loom::owners::add(&db, trusted_owner)
        .await
        .context("trusting the App owner in the trusted-owner allowlist")?;
    println!(
        "Trusted GitHub owner '{trusted_owner}' for the inbound trigger — add more in \
         Settings → Authorized GitHub owners."
    );

    // Merge (don't overwrite) the allowlist, so re-running setup never silently
    // drops owners an operator already added to loom.toml.
    let allowed_owners = merged_allowed_owners(&opts.config.config, trusted_owner);
    let updates: Vec<(&str, &str)> = vec![
        ("LOOM_GITHUB_APP_ID", app_id.as_str()),
        ("LOOM_GITHUB_APP_SLUG", conv.slug.as_str()),
        ("LOOM_GITHUB_APP_PRIVATE_KEY", conv.pem.as_str()),
        ("LOOM_GITHUB_WEBHOOK_SECRET", conv.webhook_secret.as_str()),
        ("LOOM_GITHUB_CLIENT_ID", conv.client_id.as_str()),
        ("LOOM_GITHUB_CLIENT_SECRET", conv.client_secret.as_str()),
        ("LOOM_DOMAIN", domain),
        ("LOOM_OWNER_GITHUB", owner_login),
        ("LOOM_ALLOWED_OWNERS", allowed_owners.as_str()),
    ];
    loom::loom_config::upsert(&opts.config.config, &updates)
        .context("writing the App credentials into loom.toml")?;
    println!(
        "Also wrote them, plus LOOM_DOMAIN and LOOM_OWNER_GITHUB ({owner_login}), to {} — run \
         `loom config render-env` to produce a deploy `.env` from it.",
        opts.config.config.display()
    );

    println!();
    println!("Next steps:");
    println!("  1. Install the App on the repos loom should act on:");
    println!(
        "       https://github.com/apps/{}/installations/new",
        conv.slug
    );
    println!("  2. Sign in at {base_url} with GitHub — the App's OAuth client now handles login.");
    Ok(())
}

/// The `LOOM_ALLOWED_OWNERS` value to write after adding `owner`: the owners
/// already authored in `config_path` (if any) with `owner` appended when absent
/// (case-insensitive). This makes writing the allowlist a **merge**, not an
/// overwrite, so re-running setup never silently drops previously trusted
/// owners. Returns a comma-separated list.
fn merged_allowed_owners(config_path: &std::path::Path, owner: &str) -> String {
    let existing = loom::loom_config::load(config_path)
        .ok()
        .and_then(|c| c.allowed_owners);
    let mut owners: Vec<String> = existing
        .as_deref()
        .map(|s| loom::owners::split_logins(s).map(str::to_string).collect())
        .unwrap_or_default();
    if !owners.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        owners.push(owner.to_string());
    }
    owners.join(", ")
}

/// The bare host from a `--base-url` like `https://loom.team.dev` or
/// `http://localhost:7878` — no scheme, no port. What `LOOM_DOMAIN` expects
/// (the Caddyfile in `deploy/standalone` templates it in directly).
fn host_from_base_url(base_url: &str) -> &str {
    base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':'])
        .next()
        .unwrap_or("loom")
}

/// A default App name derived from the host in `--base-url` (`loom-<host>`,
/// non-alphanumerics folded to `-`) — GitHub App names must be unique across
/// all of GitHub, so this is a starting point, not a guarantee.
fn default_app_name(base_url: &str) -> String {
    let host = host_from_base_url(base_url);
    let slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("loom-{slug}")
}

/// `loom setup secrets` — prompt for the paste-once agent secrets and store
/// them as operator environment variables (`crate::agent_env`), exported into
/// every session loom launches from then on.
async fn cmd_setup_secrets(opts: SecretsOpts) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!(
            "loom setup secrets needs an interactive terminal (hidden input for the \
             secrets you paste) — run it directly, not piped or in CI"
        );
    }
    let db = loom::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;
    // Which secrets are already stored, so the prompts can say a blank answer
    // keeps the existing value rather than clearing it.
    let existing_names: std::collections::HashSet<String> = loom::agent_env::pairs(&db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();

    println!("Paste-once secrets for the agents loom launches (leave blank to skip).");
    let anthropic = prompt_secret(
        "ANTHROPIC_API_KEY",
        existing_names.contains("ANTHROPIC_API_KEY"),
    )?;
    let gh_token = prompt_secret("GH_TOKEN", existing_names.contains("GH_TOKEN"))?;

    if anthropic.is_none() && gh_token.is_none() {
        println!("nothing entered — existing values kept unchanged");
        return Ok(());
    }

    let mut stored = Vec::new();
    if let Some(v) = &anthropic {
        loom::agent_env::set(&db, "ANTHROPIC_API_KEY", v).await?;
        stored.push("ANTHROPIC_API_KEY");
    }
    if let Some(v) = &gh_token {
        loom::agent_env::set(&db, "GH_TOKEN", v).await?;
        stored.push("GH_TOKEN");
    }
    println!();
    println!(
        "Stored {} as operator environment variables — every session launched from now \
         on gets them (Settings → Environment in the web UI, or `GET /api/env`).",
        stored.join(", ")
    );
    println!(
        "Note: the loom daemon's own process (cloning private repos, and the `gh` \
         fallback when no GitHub App is configured) still reads these ambiently — set \
         them as real environment variables before the daemon starts for that to apply."
    );

    let mut updates: Vec<(&str, &str)> = Vec::new();
    if let Some(v) = &anthropic {
        updates.push(("ANTHROPIC_API_KEY", v.as_str()));
    }
    if let Some(v) = &gh_token {
        updates.push(("GH_TOKEN", v.as_str()));
    }
    loom::loom_config::upsert(&opts.config.config, &updates)
        .context("writing the paste-once secrets into loom.toml")?;
    println!(
        "Also wrote them to {} — run `loom config render-env` then restart the daemon (e.g. \
         `docker compose up -d`) to apply the ambient-process use.",
        opts.config.config.display()
    );
    Ok(())
}

/// Ask whether the GitHub App should be owned by an organization instead of the
/// operator's personal account, returning the org login (or `None` for a
/// personal App). An org-owned App is created under the org's own developer
/// settings; the individual `LOOM_OWNER_GITHUB` is still resolved separately
/// (see [`prompt_owner`]).
fn prompt_org() -> Result<Option<String>> {
    let choice = prompt_choice(
        "Who should own the GitHub App?",
        &[
            "Your personal account",
            "An organization (its members share the App)",
        ],
        0,
    )?;
    if choice == 0 {
        return Ok(None);
    }
    let org = loop {
        let login = prompt_line("Organization login", None)?;
        if loom::owners::valid_login(&login) {
            break login;
        }
        println!("  '{login}' isn't a valid GitHub org login (letters, digits, and hyphens only).");
    };
    Ok(Some(org))
}

/// Prompt (plain, not hidden — a GitHub login isn't a secret) for the
/// individual owner login an `--org` install needs, since the org itself
/// can't be `LOOM_OWNER_GITHUB`.
fn prompt_owner(org: &str) -> Result<String> {
    use std::io::Write;
    print!(
        "The App will be owned by the {org} organization, but the first approved sign-in needs \
         your individual GitHub login (LOOM_OWNER_GITHUB) — enter it: "
    );
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading the owner login")?;
    let owner = input.trim().to_string();
    if owner.is_empty() {
        bail!("an owner GitHub login is required for an --org install");
    }
    Ok(owner)
}

/// Prompt for a secret without echoing it to the terminal. An empty answer
/// means "skip" (leave the current value, if any, alone). `already_set` annotates
/// the prompt so the operator knows a blank answer keeps the stored value.
fn prompt_secret(name: &str, already_set: bool) -> Result<Option<String>> {
    let hint = if already_set {
        " (already set — blank keeps it)"
    } else {
        ""
    };
    let value = rpassword::prompt_password(format!("{name}{hint}: "))
        .with_context(|| format!("reading {name}"))?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

// ---------------------------------------------------------------------------
// `loom config` — the typed loom.toml, and everything derived from it. The
// shared contract a deploy (e.g. deploy/gcp) builds against; see `Cmd::Config`.
// ---------------------------------------------------------------------------

async fn run_config(cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::RenderEnv(opts) => cmd_config_render_env(opts),
        ConfigCmd::SecretNames(opts) => cmd_config_secret_names(opts),
        ConfigCmd::PushSecrets(opts) => cmd_config_push_secrets(opts).await,
        ConfigCmd::Set { key, value } => cmd_config_set(key, value).await,
    }
}

/// `loom config set` — write one runtime setting straight into the sqlite
/// `settings` table, no running server needed. The direct-db counterpart to
/// `weaver config set`, which does the same thing over `PATCH /api/settings`
/// against a running daemon — the form a deploy's boot sequence needs, since
/// it must seed the auth settings *before* loom starts listening.
async fn cmd_config_set(key: String, value: String) -> Result<()> {
    if let Err(why) = weaver_core::config::validate(&key, &value) {
        bail!("{key}: {why}");
    }
    let db = loom::db::connect(&weaver_core::db::default_db_path())
        .await
        .context("opening loom's database")?;
    weaver_core::config::apply(&db, &[(key.clone(), Some(value))])
        .await
        .with_context(|| format!("writing setting '{key}'"))?;
    println!("set {key}");
    Ok(())
}

/// Warn to stderr, naming each field, when an ambient env var silently
/// outranked `loom.toml` for this run — the footgun a deploy workstation hits
/// when a personal `GH_TOKEN`/`ANTHROPIC_API_KEY` etc. happens to be exported
/// (see `loom_config::resolve_reporting_shadows`).
fn warn_shadowed_env(shadowed: &[&str], config_path: &std::path::Path) {
    for name in shadowed {
        eprintln!(
            "warning: ambient env var {name} overrides the value for {name} already set in {} \
             for this run — that's the value being rendered/pushed. Unset {name}, or edit the \
             file, if that's not what you want.",
            config_path.display()
        );
    }
}

/// `loom config render-env` — resolve `loom.toml` (plus any ambient env
/// override) and write it out as a dotenv file, the only place the
/// field→`ENV_NAME` mapping is applied.
fn cmd_config_render_env(opts: RenderEnvOpts) -> Result<()> {
    let (config, shadowed) = loom::loom_config::resolve_reporting_shadows(&opts.config.config)
        .with_context(|| format!("loading {}", opts.config.config.display()))?;
    warn_shadowed_env(&shadowed, &opts.config.config);
    let rendered = loom::loom_config::render_env(&config);
    if opts.out == "-" {
        print!("{rendered}");
    } else {
        let out = std::path::Path::new(&opts.out);
        loom::envfile::write_private(out, &rendered)
            .with_context(|| format!("writing {}", out.display()))?;
        eprintln!(
            "wrote {} from {}",
            out.display(),
            opts.config.config.display()
        );
    }
    Ok(())
}

/// `loom config secret-names` — the secret fields' `ENV_NAME`s, one per line.
/// Static (drawn from the schema, not from which fields happen to be set) —
/// what a Secret Manager provisioning step names its secrets after.
fn cmd_config_secret_names(opts: ConfigPathOpts) -> Result<()> {
    // Resolved (not just iterated statically) so a malformed loom.toml surfaces
    // here rather than only later, in render-env or push-secrets.
    loom::loom_config::resolve(&opts.config)
        .with_context(|| format!("loading {}", opts.config.display()))?;
    for field in loom::loom_config::FIELDS.iter().filter(|f| f.secret) {
        println!("{}", field.env_name);
    }
    Ok(())
}

/// `loom config push-secrets` — push every set secret field to a Secret
/// Manager backend, secret id == `ENV_NAME`. Values travel over the
/// subprocess's stdin, never a command-line argument or a log line.
async fn cmd_config_push_secrets(opts: PushSecretsOpts) -> Result<()> {
    let (config, shadowed) = loom::loom_config::resolve_reporting_shadows(&opts.config.config)
        .with_context(|| format!("loading {}", opts.config.config.display()))?;
    warn_shadowed_env(&shadowed, &opts.config.config);
    let mut pushed = Vec::new();
    let mut skipped = Vec::new();
    for field in loom::loom_config::FIELDS.iter().filter(|f| f.secret) {
        let Some(value) = field.get(&config) else {
            skipped.push(field.env_name);
            continue;
        };
        match opts.backend {
            SecretBackend::Gcp => gcp_push_secret(&opts.project, field.env_name, value).await,
        }
        .with_context(|| format!("pushing {} to Secret Manager", field.env_name))?;
        pushed.push(field.env_name);
    }
    if !pushed.is_empty() {
        println!("pushed: {}", pushed.join(", "));
    }
    if !skipped.is_empty() {
        println!("skipped (not set in loom.toml): {}", skipped.join(", "));
    }
    Ok(())
}

/// Create-or-update one GCP Secret Manager secret via the `gcloud` CLI,
/// feeding `value` over stdin so it never appears in an argument list or a
/// process listing.
async fn gcp_push_secret(project: &str, name: &str, value: &str) -> Result<()> {
    let exists = tokio::process::Command::new("gcloud")
        .args(["secrets", "describe", name, "--project", project])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("failed to spawn gcloud (is the Google Cloud SDK installed?)")?
        .success();
    let args: &[&str] = if exists {
        &[
            "secrets",
            "versions",
            "add",
            name,
            "--project",
            project,
            "--data-file=-",
        ]
    } else {
        &[
            "secrets",
            "create",
            name,
            "--project",
            project,
            "--replication-policy=automatic",
            "--data-file=-",
        ]
    };
    run_gcloud_with_stdin(args, value).await
}

/// Run `gcloud <args>`, writing `stdin_data` to its stdin and closing it —
/// the way to pass a secret value without it ever appearing in the argument
/// list (visible in `ps`) or an error message.
async fn run_gcloud_with_stdin(args: &[&str], stdin_data: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("gcloud")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn gcloud (is the Google Cloud SDK installed?)")?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(stdin_data.as_bytes())
        .await
        .context("writing the secret value to gcloud's stdin")?;
    let out = child
        .wait_with_output()
        .await
        .context("waiting for gcloud")?;
    if !out.status.success() {
        bail!(
            "gcloud {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
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
        .args(["server", "run"])
        .arg("--addr")
        .arg(&addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    let child = command.spawn().context("spawning `loom server run`")?;
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
    repo: Option<std::path::PathBuf>,
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
            repo: o.repo,
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

/// The directory a launch forks from — `--repo` if given, else the current
/// directory. `loom session launch` has no separate repo selector beyond this:
/// the server resolves the target repo from this path (its main worktree), so
/// any directory inside the intended checkout works. We canonicalize, which
/// anchors a relative `--repo` to the CLI's cwd (not the daemon's) and turns a
/// typo into a clear error here rather than an opaque server-side failure.
fn resolve_launch_cwd(repo: Option<&std::path::Path>) -> Result<std::path::PathBuf> {
    match repo {
        Some(p) => p
            .canonicalize()
            .with_context(|| format!("--repo path not found: {}", p.display())),
        None => std::env::current_dir().context("could not read the current directory"),
    }
}

async fn cmd_launch(a: LaunchArgs) -> Result<()> {
    if launch_underspecified(&a) {
        bail!("{LAUNCH_HINT}");
    }
    let LaunchArgs {
        goal,
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
    } = a;
    let client = client::default();
    let cwd = resolve_launch_cwd(repo.as_deref())?;
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
            "session {key} is orphaned — its terminal was lost (try `loom session adopt {key}`)"
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

/// `loom session preview` — print the session's recent terminal screen.
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

async fn cmd_ps(archived: bool, search: Option<String>) -> Result<()> {
    let client = client::default();
    // Hide archived sessions by default; `--search` narrows by substring. Both
    // ride the same query the dashboard uses, so the CLI and UI stay one surface.
    let mut query = Vec::new();
    if archived {
        query.push("archived=true".to_string());
    }
    if let Some(s) = search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        query.push(format!("q={}", encode_query(s)));
    }
    let path = if query.is_empty() {
        "/api/sessions".to_string()
    } else {
        format!("/api/sessions?{}", query.join("&"))
    };
    let list = client.get(&path).await?;
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        let hint = match search {
            Some(s) if !s.trim().is_empty() => format!("no sessions match '{}'", s.trim()),
            _ => "no sessions — start one with `loom session launch \"<task>\"`".to_string(),
        };
        println!("{hint}");
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

/// `loom session rename` — set a session's one-line dashboard title via
/// `PATCH /api/sessions/{key}`. The agent's parity with the dashboard's inline
/// title edit: anything the operator can do from the UI, the concierge can do too.
async fn cmd_session_rename(key: String, title: String) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!("nothing to rename to — provide a new title");
    }
    let client = client::default();
    let ws = client
        .patch(&format!("/api/sessions/{key}"), json!({ "title": title }))
        .await?;
    println!(
        "renamed {} → {}",
        str_field(&ws, "id"),
        branch_str(&ws, "title")
    );
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
    println!("  session:  {}", str_field(ws, "term_session"));
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
        .get("term_session")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("session has no terminal"))?;
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

async fn cmd_archive(key: String) -> Result<()> {
    let client = client::default();
    let res = client
        .post(&format!("/api/sessions/{key}/archive"), json!({}))
        .await?;
    println!(
        "archived {} (terminal + worktree removed; branch and history kept)",
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
    println!("  session: {}", str_field(&ws, "term_session"));
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

    #[test]
    fn host_from_base_url_strips_scheme_and_port() {
        assert_eq!(host_from_base_url("https://loom.team.dev"), "loom.team.dev");
        assert_eq!(host_from_base_url("http://localhost:7878"), "localhost");
        assert_eq!(
            host_from_base_url("https://loom.example.com/"),
            "loom.example.com"
        );
    }

    #[test]
    fn base_url_from_domain_reconstructs_the_wizard_default() {
        // A real domain → https; the stored LOOM_DOMAIN has no scheme.
        assert_eq!(
            base_url_from_domain("loom.team.dev").as_deref(),
            Some("https://loom.team.dev")
        );
        // localhost lost its port on the way to LOOM_DOMAIN → the local default.
        assert_eq!(
            base_url_from_domain("localhost").as_deref(),
            Some("http://localhost:7878")
        );
        assert_eq!(
            base_url_from_domain("127.0.0.1").as_deref(),
            Some("http://localhost:7878")
        );
        // Nothing stored → no pre-fill (caller falls back to its own default).
        assert_eq!(base_url_from_domain(""), None);
        assert_eq!(base_url_from_domain("   "), None);
    }

    #[test]
    fn default_app_name_folds_the_host_from_base_url() {
        assert_eq!(
            default_app_name("https://loom.team.dev"),
            "loom-loom-team-dev"
        );
        assert_eq!(default_app_name("http://localhost:7878"), "loom-localhost");
    }

    #[test]
    fn merged_allowed_owners_appends_without_dropping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        // No file yet → just the new owner.
        assert_eq!(merged_allowed_owners(&path, "acme"), "acme");
        // With an existing list, a new owner is appended and the rest kept.
        loom::loom_config::upsert(&path, &[("LOOM_ALLOWED_OWNERS", "org-a, org-b")]).unwrap();
        assert_eq!(merged_allowed_owners(&path, "acme"), "org-a, org-b, acme");
        // An already-present owner (any case) is not duplicated.
        assert_eq!(merged_allowed_owners(&path, "ORG-A"), "org-a, org-b");
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
            repo: None,
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
    fn resolve_launch_cwd_honors_repo_and_rejects_a_bad_path() {
        // No `--repo` falls back to the current directory.
        let here = std::env::current_dir().unwrap();
        assert_eq!(resolve_launch_cwd(None).unwrap(), here);

        // `--repo` selects (and canonicalizes) the given checkout.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_launch_cwd(Some(dir.path())).unwrap(),
            dir.path().canonicalize().unwrap()
        );

        // A typo'd `--repo` fails here, not as an opaque server error.
        assert!(resolve_launch_cwd(Some(&dir.path().join("nope"))).is_err());
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

    /// `loom config set` writes straight to the sqlite `settings` table — no
    /// HTTP, no running server — the fix for the deploy `loom-init` one-shot,
    /// which must seed the auth settings before loom starts listening.
    #[tokio::test]
    #[serial_test::serial]
    async fn config_set_writes_directly_to_sqlite_with_no_server() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());

        cmd_config_set("auth.cookie_secure".to_string(), "true".to_string())
            .await
            .unwrap();

        let db = loom::db::connect(&weaver_core::db::default_db_path())
            .await
            .unwrap();
        assert_eq!(
            weaver_core::config::get(&db, "auth.cookie_secure")
                .await
                .as_deref(),
            Some("true")
        );

        // An invalid value for a registered (bool) key is rejected before
        // touching the database.
        let err = cmd_config_set("auth.cookie_secure".to_string(), "sideways".to_string())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("auth.cookie_secure"),
            "error should name the key: {err}"
        );

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
