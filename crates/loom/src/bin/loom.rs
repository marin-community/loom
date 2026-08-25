//! loom — the orchestration CLI.
//!
//! Most subcommands talk to the running loom daemon over HTTP (session
//! lifecycle, archive, adopt). `loom server run` runs the daemon itself in the
//! foreground; `loom server start`/`stop`/`restart`/`status` manage its
//! background lifecycle. To interact with an agent, `loom sessions attach` to its
//! terminal (the browser terminal is the other interaction surface).

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgMatches, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use loom::agent_cli::{
    ArtifactCmd as AgentArtifactCmd, ChannelCmd as AgentChannelCmd, ConfigCmd as AgentConfigCmd,
    IssueCmd as AgentIssueCmd, StatusCmd as AgentStatusCmd, TagCmd as AgentTagCmd,
};
use serde_json::{json, Value};
use weaver_api::operations::permissions as perm_ops;
use weaver_api::operations::session_layout;
use weaver_api::{
    ArtifactTextAnchorDto, DecidePermissionRequestReq, MoveSessionsReq, ReviewAnchorDto,
    ReviewAnchorKindDto, ReviewSubjectKindDto, SearchSessionsOptions, SessionCreatorFilter,
    SessionLayoutItemKind, SessionLayoutView, SessionPlacementSelectorKind, SessionSearchAttention,
    SessionSearchStatus, SetSessionGithubAccessReq,
};

use loom::client::{self, Client};
use weaver_api::operations::deployment;
use weaver_api::operations::{auth, branches, mcps, profiles, reviews, sessions};
use weaver_core::db::Db;

#[derive(Parser)]
#[command(
    name = "loom",
    version,
    about = "Orchestrate concurrent agent workstreams",
    disable_help_subcommand = true
)]
struct Cli {
    /// Select a named client context for this command.
    #[arg(long, global = true)]
    context: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum HostCmd {
    /// Explore Loom's registered resource groups and operations.
    Help {
        /// Resource group or stable operation id.
        topic: Option<String>,
        /// Emit registered operation records as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compare this CLI with the connected server's live operation registry.
    Doctor,
    /// Inspect live settings through the REST API.
    Settings {
        #[command(subcommand)]
        cmd: AgentConfigCmd,
    },
    /// Inspect trusted MCP capability sets, or run an internal stdio adapter.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
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

    /// Manage watches: periodic / triggered watch programs over the fleet.
    ///
    /// A watch wakes on a trigger (a cron tick or a session event),
    /// surveys the fleet, and acts — marking a session, nudging a stuck one,
    /// escalating to you. Author one as a plain file an agent can edit, then
    /// register it and iterate with `--dry-run`:
    ///
    ///     loom watch programs                 # the builtin programs that ship with loom
    ///     loom watch new test-watch          # scaffold ~/.weaver/watches/test-watch.py
    ///     loom watch add status --cron "0 * * * *" --capabilities observe,judge,mark
    ///     loom watch run status --dry-run     # simulate; mutating actions are stubbed
    ///     loom watch enable status            # arm it
    ///     loom watch ls                       # the fleet of watchers
    Watch {
        #[command(subcommand)]
        cmd: WatchCmd,
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
    /// Authenticate this CLI and save a named client context.
    Login {
        /// Context name to create or replace.
        #[arg(default_value = "default")]
        name: String,
        /// Loom server URL. Omit to enter it interactively.
        #[arg(long)]
        url: Option<String>,
        /// Read the API token from stdin instead of a hidden prompt.
        #[arg(long)]
        token_stdin: bool,
    },
    /// Remove the saved credential for a client context.
    Logout {
        #[arg(default_value = "default")]
        name: String,
    },
    /// Manage named local and remote Loom client contexts.
    Context {
        #[command(subcommand)]
        cmd: ClientContextCmd,
    },
    /// Manage named session launch profiles and their secret environment.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Manage trusted GitHub Actions OIDC workflow mappings.
    Federation {
        #[command(subcommand)]
        cmd: FederationCmd,
    },
    /// Apply a declarative deployment manifest through Loom's REST API.
    Deployment {
        #[command(subcommand)]
        cmd: DeploymentCmd,
    },

    /// Guided one-time credential setup.
    ///
    /// `loom setup` with no subcommand runs the **interactive walkthrough**: it
    /// establishes a bootstrap operator, then optionally the GitHub App and
    /// agent secrets — one command to get a fresh instance ready. Re-running it
    /// is safe: each step pre-fills its default from the existing config. The
    /// subcommands below run an individual step directly.
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
    /// `loom setup secrets` prompts for paste-once model-provider secrets
    /// (Anthropic and OpenAI API keys) and stores them on the default profile. They
    /// apply to future sessions launched with that profile:
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
    /// `settings` table (the keys the settings pane exposes over HTTP), with
    /// no server required.
    ///
    /// `loom.toml` is the single authored source of truth for every
    /// credential/setting — the shared contract deployment tooling consumes:
    ///
    ///     loom config render-env                # -> deploy/standalone/.env
    ///     loom config secret-names               # the secret fields' ENV_NAMEs
    ///     loom config push-secrets --backend gcp --project my-project
    ///     loom config set auth.cookie_secure true  # direct-to-sqlite, no server needed
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },

    /// Open the loom web UI in a browser.
    Open,
    /// Generate shell completions.
    Completions { shell: clap_complete::Shell },
}

/// A typed invocation produced by one of Loom's CLI command groups.
#[allow(clippy::large_enum_variant)]
enum RegisteredCliCommand {
    Summary,
    SelfContext,
    Status(AgentStatusCmd),
    Sessions(SessionCmd),
    Launch(LaunchOpts),
    Ps,
    Attach(String),
    Hook(String),
    Channels(AgentChannelCmd),
    Artifacts(AgentArtifactCmd),
    Review(ReviewCmd),
    Issues(AgentIssueCmd),
    Permissions(PermissionsCmd),
    GithubToken,
}

#[allow(clippy::large_enum_variant)]
enum Cmd {
    Registered(RegisteredCliCommand),
    /// An operation reached through the generic registry dispatcher.
    ///
    /// No per-command code: the clap surface, the request, and the printing all
    /// come from the operation's own declaration.
    Operation(loom::cli::CliBinding, ArgMatches),
    Host(HostCmd),
}

/// Registry bindings whose command name no hand-written command already claims.
///
/// Both kinds coexist deliberately. A hand-written command exists because its
/// output is worth formatting by hand; the generic dispatcher exists so that
/// declaring an operation makes it reachable immediately, with no second edit.
/// `operations_reachable_from_the_command_line` counts the split, so the
/// hand-written set can shrink without anyone losing track of what is left.
fn generic_bindings() -> Vec<loom::cli::CliBinding> {
    loom::cli::bindings()
        .into_iter()
        .filter(|binding| {
            // Skip only when the hand-written surface already offers this exact
            // invocation. Groups merge; clap panics on duplicates. Test the
            // whole path, not just its head.
            binding
                .operation
                .cli
                .is_some_and(|cli| !host_tree_offers(cli.path))
        })
        .collect()
}

/// Whether the hand-written command tree already answers to `path`.
///
/// Deliberately builds the hand-written tree only. Asking `Cli::command()` would
/// recurse: that builds the full tree, which consults `generic_bindings()`,
/// which asks this.
fn host_tree_offers(path: &[&str]) -> bool {
    let root = HostCmd::augment_subcommands(hand_written_subcommands(Command::new("loom")));
    let mut node = Some(&root);
    for segment in path {
        node = node.and_then(|current| {
            current.get_subcommands().find(|candidate| {
                candidate.get_name() == *segment
                    || candidate.get_all_aliases().any(|alias| alias == *segment)
            })
        });
        if node.is_none() {
            return false;
        }
    }
    true
}

type ParseCliCommand = fn(&ArgMatches) -> clap::error::Result<RegisteredCliCommand>;

#[derive(Clone, Copy)]
struct CliCommandFactory {
    name: &'static str,
    aliases: &'static [&'static str],
    build: fn() -> Command,
    parse: ParseCliCommand,
}

impl CliCommandFactory {
    fn accepts(self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

#[derive(Args)]
struct HookArgs {
    #[arg(long)]
    event: String,
}

#[derive(Args)]
struct AttachArgs {
    session: String,
}

macro_rules! registered_subcommands {
    ($build:ident, $parse:ident, $name:literal, $about:literal, $ty:ty, $variant:path) => {
        fn $build() -> Command {
            <$ty as Subcommand>::augment_subcommands(Command::new($name).about($about))
        }

        fn $parse(matches: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
            <$ty as FromArgMatches>::from_arg_matches(matches).map($variant)
        }
    };
}

registered_subcommands!(
    status_command,
    parse_status_command,
    "status",
    "Read or update the current session's durable status.",
    AgentStatusCmd,
    RegisteredCliCommand::Status
);
registered_subcommands!(
    sessions_command,
    parse_sessions_command,
    "sessions",
    "Launch, inspect, and drive concurrent sessions.",
    SessionCmd,
    RegisteredCliCommand::Sessions
);
registered_subcommands!(
    channels_command,
    parse_channels_command,
    "channels",
    "Read and write durable conversation channels.",
    AgentChannelCmd,
    RegisteredCliCommand::Channels
);
registered_subcommands!(
    artifacts_command,
    parse_artifacts_command,
    "artifacts",
    "Read and write named, versioned deliverables.",
    AgentArtifactCmd,
    RegisteredCliCommand::Artifacts
);
registered_subcommands!(
    review_command,
    parse_review_command,
    "review",
    "Draft and submit one coherent artifact review into a session.",
    ReviewCmd,
    RegisteredCliCommand::Review
);
registered_subcommands!(
    issues_command,
    parse_issues_command,
    "issues",
    "Manage session-owned and repository backlog work items.",
    AgentIssueCmd,
    RegisteredCliCommand::Issues
);
registered_subcommands!(
    permissions_command,
    parse_permissions_command,
    "permissions",
    "Inspect effective access, request expansion, or decide requests.",
    PermissionsCmd,
    RegisteredCliCommand::Permissions
);

fn summary_command() -> Command {
    Command::new("summary").about("Print a capability-aware catch-up for the current session.")
}

fn parse_summary_command(_: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    Ok(RegisteredCliCommand::Summary)
}

fn self_command() -> Command {
    Command::new("self").about("Resolve the current session, repository, channel, and links.")
}

fn parse_self_command(_: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    Ok(RegisteredCliCommand::SelfContext)
}

fn launch_command() -> Command {
    LaunchOpts::augment_args(
        Command::new("launch").about("Launch a new session; shortcut for `loom sessions launch`."),
    )
}

fn parse_launch_command(matches: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    LaunchOpts::from_arg_matches(matches).map(RegisteredCliCommand::Launch)
}

fn ps_command() -> Command {
    Command::new("ps").about("List active sessions; shortcut for `loom sessions list`.")
}

fn parse_ps_command(_: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    Ok(RegisteredCliCommand::Ps)
}

fn attach_command() -> Command {
    AttachArgs::augment_args(
        Command::new("attach").about("Attach to a session; shortcut for `loom sessions attach`."),
    )
}

fn parse_attach_command(matches: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    AttachArgs::from_arg_matches(matches).map(|args| RegisteredCliCommand::Attach(args.session))
}

fn hook_command() -> Command {
    HookArgs::augment_args(Command::new("hook").hide(true))
}

fn parse_hook_command(matches: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    HookArgs::from_arg_matches(matches).map(|args| RegisteredCliCommand::Hook(args.event))
}

fn github_token_command() -> Command {
    Command::new("github-token").hide(true)
}

fn parse_github_token_command(_: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    Ok(RegisteredCliCommand::GithubToken)
}

const SESSION_CLI_COMMANDS: &[CliCommandFactory] = &[
    CliCommandFactory {
        name: "summary",
        aliases: &[],
        build: summary_command,
        parse: parse_summary_command,
    },
    CliCommandFactory {
        name: "self",
        aliases: &[],
        build: self_command,
        parse: parse_self_command,
    },
    CliCommandFactory {
        name: "status",
        aliases: &[],
        build: status_command,
        parse: parse_status_command,
    },
    CliCommandFactory {
        name: "sessions",
        aliases: &["session"],
        build: sessions_command,
        parse: parse_sessions_command,
    },
    CliCommandFactory {
        name: "launch",
        aliases: &[],
        build: launch_command,
        parse: parse_launch_command,
    },
    CliCommandFactory {
        name: "ps",
        aliases: &[],
        build: ps_command,
        parse: parse_ps_command,
    },
    CliCommandFactory {
        name: "attach",
        aliases: &[],
        build: attach_command,
        parse: parse_attach_command,
    },
    CliCommandFactory {
        name: "hook",
        aliases: &[],
        build: hook_command,
        parse: parse_hook_command,
    },
];

const CHANNEL_CLI_COMMANDS: &[CliCommandFactory] = &[CliCommandFactory {
    name: "channels",
    aliases: &[],
    build: channels_command,
    parse: parse_channels_command,
}];

const ARTIFACT_CLI_COMMANDS: &[CliCommandFactory] = &[
    CliCommandFactory {
        name: "artifacts",
        aliases: &[],
        build: artifacts_command,
        parse: parse_artifacts_command,
    },
    CliCommandFactory {
        name: "review",
        aliases: &[],
        build: review_command,
        parse: parse_review_command,
    },
];

const ISSUE_CLI_COMMANDS: &[CliCommandFactory] = &[CliCommandFactory {
    name: "issues",
    aliases: &[],
    build: issues_command,
    parse: parse_issues_command,
}];

const PERMISSION_CLI_COMMANDS: &[CliCommandFactory] = &[
    CliCommandFactory {
        name: "permissions",
        aliases: &[],
        build: permissions_command,
        parse: parse_permissions_command,
    },
    CliCommandFactory {
        name: "github-token",
        aliases: &[],
        build: github_token_command,
        parse: parse_github_token_command,
    },
];

const CLI_COMMAND_GROUPS: &[&[CliCommandFactory]] = &[
    SESSION_CLI_COMMANDS,
    CHANNEL_CLI_COMMANDS,
    ARTIFACT_CLI_COMMANDS,
    ISSUE_CLI_COMMANDS,
    PERMISSION_CLI_COMMANDS,
];

fn registered_cli_factory(name: &str) -> Option<&'static CliCommandFactory> {
    CLI_COMMAND_GROUPS
        .iter()
        .flat_map(|commands| commands.iter())
        .find(|factory| factory.accepts(name))
}

impl FromArgMatches for Cmd {
    fn from_arg_matches(matches: &ArgMatches) -> clap::error::Result<Self> {
        if let Some((name, command_matches)) = matches.subcommand() {
            if let Some(factory) = registered_cli_factory(name) {
                return (factory.parse)(command_matches).map(Self::Registered);
            }
        }
        let bindings = generic_bindings();
        if let Some((binding, operation_matches)) = loom::cli::resolve(&bindings, matches) {
            return Ok(Self::Operation(*binding, operation_matches));
        }
        HostCmd::from_arg_matches(matches).map(Self::Host)
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> clap::error::Result<()> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Subcommand for Cmd {
    // Registry-derived commands merge into existing groups last. Earlier
    // placement would create duplicates that clap rejects.
    fn augment_subcommands(command: Command) -> Command {
        augment_registered_subcommands(HostCmd::augment_subcommands(hand_written_subcommands(
            command,
        )))
    }

    fn augment_subcommands_for_update(command: Command) -> Command {
        augment_registered_subcommands(HostCmd::augment_subcommands_for_update(
            hand_written_subcommands(command),
        ))
    }

    fn has_subcommand(name: &str) -> bool {
        registered_cli_factory(name).is_some() || HostCmd::has_subcommand(name)
    }
}

/// Merge the registry-derived commands into an already-assembled tree.
fn augment_registered_subcommands(command: Command) -> Command {
    loom::cli::augment(command, &generic_bindings())
}

/// The hand-written registered commands, without anything registry-derived.
fn hand_written_subcommands(mut command: Command) -> Command {
    for commands in CLI_COMMAND_GROUPS {
        for factory in *commands {
            let mut registered = (factory.build)();
            for alias in factory.aliases {
                registered = registered.alias(*alias);
            }
            command = command.subcommand(registered);
        }
    }
    command
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

/// Subcommands under `loom review`.
#[derive(Subcommand)]
enum ReviewCmd {
    /// List reviews for one artifact in a session.
    Ls { session: String, artifact: String },
    /// Show the exact server-authoritative review envelope and delivery preview.
    Show { review_id: i64 },
    /// Add a pending comment, creating the caller's draft when needed.
    Add {
        session: String,
        artifact: String,
        #[arg(long)]
        rev: i64,
        #[arg(long)]
        quote: String,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "")]
        suffix: String,
        #[arg(long)]
        block: Option<i64>,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Edit a pending comment body.
    Edit {
        review_id: i64,
        comment_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Move a pending comment to a new text/block anchor and revision.
    Reanchor {
        review_id: i64,
        comment_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(long)]
        rev: i64,
        #[arg(long)]
        quote: String,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "")]
        suffix: String,
        #[arg(long)]
        block: Option<i64>,
    },
    /// Create or update an overall-note-only draft.
    Overall {
        session: String,
        artifact: String,
        #[arg(long)]
        rev: i64,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Delete one pending comment.
    DeleteComment {
        review_id: i64,
        comment_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
    },
    /// Resolve one submitted review comment.
    Resolve { review_id: i64, comment_id: i64 },
    /// Reopen one resolved review comment.
    Reopen { review_id: i64, comment_id: i64 },
    /// Discard a draft and all of its pending comments.
    Discard {
        review_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
    },
    /// Move an overall-only draft target to the artifact's current revision.
    Retarget {
        review_id: i64,
        /// Draft revision shown by `loom review show` or the previous mutation.
        #[arg(long)]
        revision: i64,
    },
    /// Submit the immutable review and enqueue one structured conversation message.
    Submit {
        review_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(long, default_value = "")]
        summary: String,
        /// Intentionally submit anchors from an older artifact revision.
        #[arg(long)]
        acknowledge_outdated: bool,
    },
    /// Retry a failed review delivery.
    Retry { review_id: i64 },
}

/// Subcommands under `loom sessions` — the uniform way to drive a child session.
// `Launch` carries the flattened `LaunchOpts` arg struct, which clap derives
// against by value — boxing it (clippy's `large_enum_variant` suggestion) would
// fight the `Subcommand` derive. This is a short-lived CLI dispatch enum, so the
// size skew is harmless.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum SessionCmd {
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

#[derive(Subcommand)]
enum PermissionsCmd {
    /// Show effective Loom operations, GitHub scope, and pending requests.
    Show {
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
        /// Emit the typed response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain a registered operation's actor, risk, and projections.
    Explain { operation: String },
    /// Request a human-approved expansion of this session's external access.
    Request {
        #[command(subcommand)]
        resource: PermissionRequestResource,
    },
    /// List durable access requests for a session.
    Requests {
        #[arg(long)]
        session: Option<String>,
        /// pending, approved, or denied. Omit to list all.
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Approve and apply one pending request (human operator only).
    Approve {
        request: String,
        /// Optional audit reason. Multiple words are joined.
        reason: Vec<String>,
    },
    /// Deny one pending request (human operator only).
    Deny {
        request: String,
        /// Optional audit reason. Multiple words are joined.
        reason: Vec<String>,
    },
    /// Directly grant external access without a prior request (human only).
    Grant {
        #[command(subcommand)]
        resource: PermissionGrantResource,
    },
    /// Revoke an explicit external-access override (human only).
    Revoke {
        #[command(subcommand)]
        resource: PermissionGrantResource,
    },
}

#[derive(Subcommand)]
enum PermissionRequestResource {
    /// Ask for GitHub App write access to one repository.
    GithubRepository {
        repository: String,
        /// Why the task needs this repository.
        #[arg(long, required = true)]
        reason: String,
        #[arg(long, default_value = "write")]
        mode: String,
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum PermissionGrantResource {
    /// Grant or revoke GitHub App write access to one repository.
    GithubRepository {
        repository: String,
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionLayoutCmd {
    /// Print spaces, groups, ordered sessions, defaults, and the revision.
    Show,
    /// Add a space with an empty Inbox group.
    SpaceAdd {
        name: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Rename a space.
    SpaceRename {
        id: String,
        name: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Delete a space, moving any contents/defaults to `--to`.
    SpaceDelete {
        id: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Add an empty group to a space.
    GroupAdd {
        space: String,
        name: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Rename a group.
    GroupRename {
        id: String,
        name: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Delete a group, moving any contents/defaults to `--to`.
    GroupDelete {
        id: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Move one space or group before an anchor (omit `--before` for the end).
    Reorder {
        kind: SessionLayoutItemKind,
        id: String,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        space: Option<String>,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Atomically move sessions into a group.
    Move {
        #[arg(long)]
        to: String,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        revision: Option<i64>,
        #[arg(required = true)]
        sessions: Vec<String>,
    },
    /// Atomically restore complete group orders from a JSON snapshot.
    Restore {
        /// JSON array of {"group_id":"…","session_ids":["…"]} objects.
        snapshot: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Collapse one group for the current operator.
    Collapse { group: String },
    /// Expand one group for the current operator.
    Expand { group: String },
    /// Set a configurable origin/profile placement default.
    DefaultSet {
        kind: SessionPlacementSelectorKind,
        value: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        revision: Option<i64>,
    },
    /// Remove a configurable placement default.
    DefaultDelete {
        kind: SessionPlacementSelectorKind,
        value: String,
        #[arg(long)]
        revision: Option<i64>,
    },
}

/// Subcommands under `loom watch` — the operator + authoring surface. A
/// thin client over the REST API ("the API is the CLI").
#[derive(Subcommand)]
enum WatchCmd {
    /// Scaffold a starter program file at `~/.weaver/watches/<name>.py`.
    ///
    /// Writes a commented Python template against the program contract (the
    /// fleet over `$WEAVER_API`, round config in `$WEAVER_WATCH`, result
    /// JSON on stdout), then prints the path. Edit it, then register it with
    /// `loom watch add <name> --program <path>`.
    New {
        /// The watch name; also the file stem (`<name>.py`).
        name: String,
    },
    /// List the builtin programs that ship with loom (`watches.programs`).
    Programs {
        /// Print one program's script source instead of the table, e.g.
        /// `--source builtin:archive-merged` — a working example to start from.
        #[arg(long)]
        source: Option<String>,
    },
    /// Register a watch from flags (`watches.create`).
    Add(Box<AddOpts>),
    /// Remove a watch.
    Rm {
        /// Watch id or name.
        name: String,
    },
    /// Enable a watch (arm it).
    Enable {
        /// Watch id or name.
        name: String,
    },
    /// Disable a watch (stop it cold, no redeploy).
    Disable {
        /// Watch id or name.
        name: String,
    },
    /// List every watch: name, enabled, trigger, program, last outcome.
    Ls,
    /// Fire a round now and print its outcome + summary.
    Run {
        /// Watch id or name.
        name: String,
        /// Simulate: every mutating action is stubbed and logged as "would do
        /// X", nothing is performed. Safe to repeat — the iteration primitive.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show a watch's round history (time, reason, outcome, summary).
    Runs {
        /// Watch id or name.
        name: String,
        /// How many recent rounds to show.
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// Show the actions each recent round took (a verbose `runs`).
    Logs {
        /// Watch id or name.
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
    /// Prompt for and store default-profile model-provider secrets.
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
/// rendered/pushed from it. The contract deployment tooling builds
/// against — see [`Cmd::Config`]. `render-env` and `push-secrets` resolve
/// every field from `loom.toml` *or* a same-named env var (env wins) — set
/// one to override a single invocation without editing the file.
///
/// `set` writes directly to the runtime `settings` table
/// (`weaver_core::config::REGISTRY`), bypassing `loom.toml`. Written to the
/// daemon's sqlite database with no server needed. This is what
/// `deploy/standalone/docker-compose.yml`'s `loom-init` uses to seed the
/// security-relevant auth settings before loom starts listening.
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
    /// running server needed.
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
    /// Mint a short-lived automation-only JWT.
    Mint {
        #[arg(long)]
        subject: String,
        #[arg(long = "profile", required = true)]
        profiles: Vec<String>,
        /// Lifetime such as `10m`, `1h`, or seconds.
        #[arg(long, default_value = "10m")]
        ttl: String,
    },
}

#[derive(Subcommand)]
enum ClientContextCmd {
    /// List configured contexts without exposing their credentials.
    Ls,
    /// Set the default context.
    Use { name: String },
    /// Add or update an endpoint without storing a credential.
    Add {
        name: String,
        #[arg(long)]
        url: String,
        /// Also make this the default context.
        #[arg(long = "use")]
        use_context: bool,
    },
    /// Show the context selected for the current directory.
    Current,
    /// Remove a context and its saved credential.
    Rm { name: String },
}

#[derive(Args)]
struct FederationAddArgs {
    /// Stable mapping name. When omitted, one is derived from identity fields.
    name: Option<String>,
    #[arg(long, default_value = "github")]
    provider: String,
    #[arg(long, default_value = "https://token.actions.githubusercontent.com")]
    issuer: String,
    #[arg(long)]
    audience: String,
    #[arg(long)]
    subject: Option<String>,
    #[arg(long)]
    service_account: Option<String>,
    #[arg(long, default_value = "github-actions")]
    service_tag: String,
    #[arg(long)]
    repository_id: Option<String>,
    #[arg(long)]
    workflow_ref: Option<String>,
    #[arg(long)]
    event: Option<String>,
    #[arg(long = "ref")]
    git_ref: Option<String>,
    #[arg(long = "profile", required = true)]
    profiles: Vec<String>,
}

#[derive(Subcommand)]
enum FederationCmd {
    Add(Box<FederationAddArgs>),
    Ls,
    Rm { id: String },
}

#[derive(Subcommand)]
enum DeploymentCmd {
    /// Reconcile settings, profiles, secret references, and workload federation mappings.
    Apply {
        /// YAML (or JSON) manifest path, or `-` for stdin.
        #[arg(long, default_value = "-")]
        file: String,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// List trusted MCP adapters and capability sets.
    Ls,
    /// Show one versioned capability set by name.
    Show { name: String },
    /// Add or replace an operator-authored uv MCP script.
    Add(Box<McpAddOpts>),
    /// Remove an operator-authored MCP definition.
    Rm { identity: String },
    /// Run one trusted stdio adapter (used only by Loom's agent runtime).
    #[command(hide = true)]
    Serve { adapter: String },
    /// Run an exact custom source snapshot (used only by the agent runtime).
    #[command(hide = true)]
    ServeCustom { identity: String },
}

#[derive(Args)]
struct McpAddOpts {
    /// Absolute identity, for example /engineering/search/docs.
    identity: String,
    #[arg(long)]
    label: String,
    #[arg(long, default_value = "")]
    description: String,
    /// Python script containing PEP 723 inline dependencies.
    #[arg(long)]
    file: String,
    /// Optional uv Python test script.
    #[arg(long)]
    tests: Option<String>,
    #[arg(long)]
    disabled: bool,
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// Add a named launch profile.
    Add(Box<ProfileAddOpts>),
    /// List profiles (secret values are never returned).
    Ls,
    /// Show one profile.
    Show {
        name: String,
        /// Resolve the exact runtime permissions and MCP processes.
        #[arg(long)]
        effective: bool,
    },
    /// Resolve the exact launch snapshot, including provenance and capacity.
    Resolve {
        name: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        protocol: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        class: Option<String>,
    },
    /// Save a resolved profile selection as a new insert-only template.
    ///
    /// Loom previews first and guards both the source profile and resolver
    /// revisions; `--copy-environment` participates in the same transaction.
    Clone {
        source: String,
        name: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        protocol: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        class: Option<String>,
        /// Copy the source's write-only environment in the clone transaction.
        #[arg(long)]
        copy_environment: bool,
        /// Remove an inherited environment name (repeatable).
        #[arg(long = "remove-environment")]
        remove_environment: Vec<String>,
        /// Add or replace a literal environment value as NAME=VALUE.
        #[arg(long = "set-environment")]
        set_environment: Vec<String>,
        /// Add or replace a Secret Manager reference as NAME=REFERENCE.
        #[arg(long = "secret-environment")]
        secret_environment: Vec<String>,
    },
    /// Remove an unused profile (`default` is protected).
    Rm { name: String },
    /// Manage a profile's write-only environment.
    Env {
        #[command(subcommand)]
        cmd: ProfileEnvCmd,
    },
}

#[derive(Args)]
struct ProfileAddOpts {
    name: String,
    #[arg(long, default_value = "")]
    description: String,
    #[arg(long)]
    agent: String,
    #[arg(long, default_value = "")]
    model: String,
    #[arg(long, default_value = "")]
    effort: String,
    #[arg(long, default_value = "")]
    protocol: String,
    #[arg(long, default_value = "auto")]
    mode: String,
    #[arg(long, default_value = "interactive")]
    class: String,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    env_clear: bool,
    #[arg(long, value_delimiter = ',')]
    ambient: Vec<String>,
    #[arg(long)]
    idle_archive_secs: Option<i64>,
    #[arg(long, default_value_t = 0)]
    max_concurrent: i64,
    #[arg(long)]
    turn_budget: Option<i64>,
    /// Prelude injected before the task: `weaver` or `none`.
    #[arg(long, default_value = "weaver")]
    prelude: String,
    /// Markdown instructions appended to the opening prompt.
    #[arg(long)]
    instructions_file: Option<String>,
    /// Apply Loom's restricted automation security posture.
    #[arg(long)]
    restricted: bool,
    /// Provider runtime permission rules (deprecated; use --mcp).
    #[arg(
        long = "runtime-permission",
        visible_alias = "allowed-tool",
        value_delimiter = ','
    )]
    runtime_permission: Vec<String>,
    /// MCP access mode: none, all, or a comma-separated group list.
    #[arg(long, default_value = "none")]
    mcp: String,
}

#[derive(Subcommand)]
enum ProfileEnvCmd {
    /// Set a write-only environment value.
    Set {
        profile: String,
        name: String,
        value: String,
    },
    /// Set a write-only GCP Secret Manager version reference.
    Secret {
        profile: String,
        name: String,
        secret_ref: String,
    },
    /// Remove an environment value.
    Rm { profile: String, name: String },
}

/// Options for `loom watch add` — the flags build the trigger / scope /
/// program / capability set the `watches.create` operation takes.
#[derive(Args)]
struct AddOpts {
    /// The watch name (unique).
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
    /// Pin the watch to one repository (filters the trigger + scope).
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
    /// Comma-separated capability set (default `observe,judge,mark` for
    /// `builtin:status`).
    /// Drawn from observe, judge, mark, escalate, nudge, interrupt, launch.
    #[arg(long, value_delimiter = ',')]
    capabilities: Option<Vec<String>>,
    /// Automation-safe ACP profile for agent judgements (default `watch`).
    #[arg(long)]
    profile: Option<String>,
    /// Model override for agent judgement calls.
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

/// Shared `launch` options, used by both `loom sessions launch` and the
/// top-level `loom launch` shortcut.
#[derive(Args)]
struct LaunchOpts {
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

#[cfg(test)]
mod cli_tree_tests {
    use super::*;
    use clap::CommandFactory;

    /// The real command tree assembles.
    ///
    /// clap validates duplicate subcommand names in a debug assertion that only
    /// fires when the tree is actually built, so nothing short of building it
    /// catches a collision between a registry-derived command and a
    /// hand-written one. The parity tests over `bindings()` cannot: they never
    /// see the host commands.
    #[test]
    fn the_command_tree_builds_without_collisions() {
        Cli::command().debug_assert();
    }

    /// Every advertised invocation exists in the tree the binary really uses.
    /// it violated: a descriptor said `cli: Some("loom issues list")` beside a
    /// clap enum whose variant was `Ls`, and three advertised commands did not
    /// exist at all. Checking bindings against the registry is not enough —
    /// both can agree while the binary offers a different word.
    ///
    /// It covers hand-written commands too, deliberately. An operation served by
    /// a bespoke command still has to be reachable by the name it advertises.
    #[test]
    fn every_advertised_invocation_exists_in_the_real_tree() {
        let command = Cli::command();
        let mut drift = Vec::new();
        for operation in weaver_api::operations::operations() {
            let Some(cli) = operation.cli else { continue };
            let mut node = Some(&command);
            for segment in cli.path {
                node = node.and_then(|current| {
                    current.get_subcommands().find(|candidate| {
                        candidate.get_name() == *segment
                            || candidate.get_all_aliases().any(|alias| alias == *segment)
                    })
                });
                if node.is_none() {
                    drift.push(format!(
                        "  {} advertises `{}` — `{segment}` is not there",
                        operation.id,
                        cli.invocation()
                    ));
                    break;
                }
            }
        }
        assert!(
            drift.is_empty(),
            "{} operation(s) advertise a command line the binary does not offer:\n{}",
            drift.len(),
            drift.join("\n")
        );
    }

    #[test]
    fn every_generic_binding_is_reachable_in_the_real_tree() {
        let command = Cli::command();
        for binding in generic_bindings() {
            let Some(cli) = binding.operation.cli else {
                continue;
            };
            let mut node = &command;
            for segment in cli.path {
                node = node
                    .get_subcommands()
                    .find(|candidate| {
                        candidate.get_name() == *segment
                            || candidate.get_all_aliases().any(|alias| alias == *segment)
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{} advertises `{}` but `{segment}` is not in the built tree",
                            binding.operation.id,
                            cli.invocation()
                        )
                    });
            }
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let Cli { context, cmd } = Cli::parse();
    client::set_context_override(context.as_deref())?;
    match cmd {
        Cmd::Registered(command) => run_registered_cli(command).await,
        Cmd::Operation(binding, matches) => {
            configure_agent_client()?;
            let rendered = (binding.run)(&matches).await?;
            if !rendered.is_empty() {
                println!("{}", rendered.trim_end());
            }
            Ok(())
        }
        Cmd::Host(command) => run_host_cli(command).await,
    }
}

async fn run_registered_cli(command: RegisteredCliCommand) -> Result<()> {
    match command {
        RegisteredCliCommand::Summary => {
            configure_agent_client()?;
            loom::agent_cli::run_summary().await
        }
        RegisteredCliCommand::SelfContext => {
            configure_agent_client()?;
            loom::agent_cli::run_self().await
        }
        RegisteredCliCommand::Status(cmd) => {
            configure_agent_client()?;
            loom::agent_cli::run_status(cmd).await
        }
        RegisteredCliCommand::Sessions(cmd) => run_session(cmd).await,
        RegisteredCliCommand::Launch(opts) => cmd_launch(opts.into()).await,
        RegisteredCliCommand::Ps => cmd_ps(PsOptions::default()).await,
        RegisteredCliCommand::Attach(session) => cmd_attach(session).await,
        RegisteredCliCommand::Hook(event) => {
            configure_agent_client()?;
            loom::agent_cli::run_hook(event).await
        }
        RegisteredCliCommand::Channels(cmd) => {
            configure_agent_client()?;
            loom::agent_cli::run_channel(cmd).await
        }
        RegisteredCliCommand::Artifacts(cmd) => {
            configure_agent_client()?;
            loom::agent_cli::run_artifact(cmd).await
        }
        RegisteredCliCommand::Review(cmd) => run_review(cmd).await,
        RegisteredCliCommand::Issues(cmd) => {
            configure_agent_client()?;
            loom::agent_cli::run_issue(cmd).await
        }
        RegisteredCliCommand::Permissions(cmd) => run_permissions(cmd).await,
        RegisteredCliCommand::GithubToken => {
            configure_agent_client()?;
            loom::agent_cli::run_github_token().await
        }
    }
}

async fn run_host_cli(command: HostCmd) -> Result<()> {
    match command {
        HostCmd::Help { topic, json } => run_help(topic, json),
        HostCmd::Doctor => run_doctor().await,
        HostCmd::Settings { cmd } => {
            configure_agent_client()?;
            loom::agent_cli::run_settings(cmd).await
        }
        HostCmd::Mcp { cmd } => run_mcp(cmd).await,
        HostCmd::Server { cmd } => run_server(cmd).await,
        HostCmd::Watch { cmd } => run_watch(cmd).await,
        HostCmd::Token { cmd } => run_token(cmd).await,
        HostCmd::Login {
            name,
            url,
            token_stdin,
        } => cmd_login(name, url, token_stdin).await,
        HostCmd::Logout { name } => cmd_logout(name),
        HostCmd::Context { cmd } => run_client_context(cmd),
        HostCmd::Profile { cmd } => run_profile(cmd).await,
        HostCmd::Federation { cmd } => run_federation(cmd).await,
        HostCmd::Deployment { cmd } => run_deployment(cmd).await,
        HostCmd::Setup { cmd } => run_setup(cmd).await,
        HostCmd::Config { cmd } => run_config(cmd).await,
        HostCmd::Open => cmd_open().await,
        HostCmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "loom", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn configure_agent_client() -> Result<()> {
    loom::agent_cli::set_client_override(client::default()?)
}

fn run_help(topic: Option<String>, as_json: bool) -> Result<()> {
    let operations: Vec<_> = match topic.as_deref() {
        None => weaver_api::operations().collect(),
        Some(topic) => {
            let matches: Vec<_> = weaver_api::operations()
                .filter(|operation| operation.bundle == topic || operation.id == topic)
                .collect();
            if matches.is_empty() {
                bail!("unknown Loom resource group or operation '{topic}' — run `loom help`");
            }
            matches
        }
    };
    if as_json {
        let views: Vec<weaver_api::OperationView> = operations
            .into_iter()
            .map(weaver_api::OperationView::from)
            .collect();
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(());
    }
    if let Some(topic) = topic {
        println!("{topic}\n");
        for operation in operations {
            let command = operation
                .cli
                .map(|cli| cli.invocation())
                .unwrap_or_else(|| "(API only)".to_string());
            println!("  {command:<58} {}", operation.summary);
            println!(
                "    id: {} · actor: {} · scope: {} · risk: {} · {} {}",
                operation.id,
                operation.actor.as_str(),
                operation.scope.as_str(),
                operation.risk.as_str(),
                operation.method(),
                operation.path()
            );
        }
        return Ok(());
    }

    println!("Loom's registered operation groups\n");
    for bundle in weaver_api::operation_bundles() {
        println!(
            "  {:<14} {} ({} operations)",
            bundle.name,
            bundle.label,
            bundle.operations.len()
        );
    }
    println!(
        "\nHost and fleet commands\n  server, setup, auth/login, context, profiles, mcp, watches, review"
    );
    println!("\nExplore with `loom help <group>`, `loom <group> --help`, or `loom help --json`.");
    Ok(())
}

async fn run_doctor() -> Result<()> {
    let endpoint = client::current_selection()?;
    let client = client::default()?;
    let meta = client.api_meta().await.with_context(|| {
        format!(
            "cannot reach Loom at {}; check `loom server status` or the selected context",
            endpoint.base
        )
    })?;
    let operations = client.operations().await?;
    let client_version = env!("CARGO_PKG_VERSION");
    println!("endpoint:             {}", endpoint.base);
    println!("client version:       {client_version}");
    println!("server version:       {}", meta.version);
    println!(
        "operation registry:   v{} ({} operations)",
        meta.operation_registry_version,
        operations.len()
    );
    if meta.version == client_version {
        println!("compatibility:        exact version match");
    } else {
        println!(
            "compatibility:        version mismatch; use `loom help --json` for compiled support and {} for the live server",
            meta.operations_url
        );
    }
    Ok(())
}

async fn run_permissions(cmd: PermissionsCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        PermissionsCmd::Show { session, json } => {
            let session = github_access_session(session)?;
            let view = client
                .invoke::<perm_ops::effective::get::Op>(&perm_ops::effective::get::Input {
                    session,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!("session: {}", view.session_id);
                println!("actor:   {}", view.actor);
                println!("operations ({}):", view.operations.len());
                for operation in view.operations {
                    println!("  {operation}");
                }
                println!("GitHub repositories ({}):", view.github_repositories.len());
                for repository in view.github_repositories {
                    println!("  {repository}");
                }
                println!("pending requests ({}):", view.pending_requests.len());
                for request in view.pending_requests {
                    println!(
                        "  {}  {} {} — {}",
                        request.id, request.mode, request.repository, request.reason
                    );
                }
            }
            Ok(())
        }
        PermissionsCmd::Explain { operation } => {
            let operation = client
                .invoke::<perm_ops::explain::Op>(&perm_ops::explain::Input { operation })
                .await
                .with_context(|| {
                    "unknown operation — run `loom help --json` to list operation ids"
                })?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
            Ok(())
        }
        PermissionsCmd::Request { resource } => match resource {
            PermissionRequestResource::GithubRepository {
                repository,
                reason,
                mode,
                session,
            } => {
                let session = github_access_session(session)?;
                let request = client
                    .invoke::<perm_ops::requests::create::Op>(&perm_ops::requests::create::Input {
                        repository,
                        reason,
                        mode,
                        session,
                    })
                    .await?;
                println!(
                    "request {} pending — {} {}",
                    request.id, request.mode, request.repository
                );
                Ok(())
            }
        },
        PermissionsCmd::Requests {
            session,
            state,
            json,
        } => {
            let session = github_access_session(session)?;
            let requests = client
                .invoke::<perm_ops::requests::list::Op>(&perm_ops::requests::list::Input {
                    state,
                    session,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&requests)?);
            } else if requests.is_empty() {
                println!("(no permission requests)");
            } else {
                for request in requests {
                    println!(
                        "{}  {:8} {} {} — {}",
                        request.id, request.state, request.mode, request.repository, request.reason
                    );
                }
            }
            Ok(())
        }
        PermissionsCmd::Approve { request, reason } => {
            let decided = client
                .decide_permission_request(
                    &request,
                    &DecidePermissionRequestReq {
                        decision: "approve".to_string(),
                        reason: reason.join(" "),
                    },
                )
                .await?;
            println!("approved {} — {}", decided.id, decided.repository);
            Ok(())
        }
        PermissionsCmd::Deny { request, reason } => {
            let decided = client
                .decide_permission_request(
                    &request,
                    &DecidePermissionRequestReq {
                        decision: "deny".to_string(),
                        reason: reason.join(" "),
                    },
                )
                .await?;
            println!("denied {} — {}", decided.id, decided.repository);
            Ok(())
        }
        PermissionsCmd::Grant { resource } => {
            update_permission_resource(&client, resource, "write").await
        }
        PermissionsCmd::Revoke { resource } => {
            update_permission_resource(&client, resource, "none").await
        }
    }
}

async fn update_permission_resource(
    client: &Client,
    resource: PermissionGrantResource,
    mode: &str,
) -> Result<()> {
    match resource {
        PermissionGrantResource::GithubRepository {
            repository,
            session,
        } => {
            let session = github_access_session(session)?;
            let view = client
                .set_session_github_access(
                    &session,
                    &SetSessionGithubAccessReq {
                        repository,
                        mode: mode.to_string(),
                    },
                )
                .await?;
            println!("{} {} — {}", view.mode, view.repository, view.granted_by);
            Ok(())
        }
    }
}

/// Why a server must not start here, if it must not.
///
/// A Loom session reaches its host loom over the API; its `WEAVER_HOME` is the
/// host's own home, so a server started inside one opens that database and
/// those supervisor sockets a second time. The machine then runs two monitors,
/// two Slack clients, and two sets of lifecycle operations on the same rows —
/// and an operation owned by the session's process dies the instant it tears
/// that session's supervisor down, stranding the transition it published.
///
/// The signal that a home already belongs to a loom is its `loom.json` state
/// file. A private `WEAVER_HOME` has none, so the documented way to exercise
/// loom by hand (`WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0`)
/// still works.
fn nested_server_refusal(session_id: Option<&str>, home: &std::path::Path) -> Option<String> {
    let session_id = session_id.filter(|id| !id.is_empty())?;
    let state = home.join("loom.json");
    if !state.exists() {
        return None;
    }
    Some(format!(
        "refusing to start: this is Loom session {session_id}, and {} already belongs to a running loom. A second server on one home races the host's monitor, Slack client, and session teardown. Run `WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0` for an isolated instance.",
        home.display()
    ))
}

/// Dispatch the `loom server <verb>` daemon-lifecycle subcommands.
async fn run_server(cmd: ServerCmd) -> Result<()> {
    if matches!(cmd, ServerCmd::Run { .. } | ServerCmd::Start) {
        if let Some(refusal) = nested_server_refusal(
            std::env::var("LOOM_SESSION_ID").ok().as_deref(),
            &loom::db::weaver_home(),
        ) {
            bail!("{refusal}");
        }
    }
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

async fn run_review(cmd: ReviewCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        ReviewCmd::Ls { session, artifact } => {
            let reviews = client
                .invoke::<reviews::list::Op>(&reviews::list::Input {
                    subject_kind: "artifact".parse().map_err(anyhow::Error::msg)?,
                    subject_key: artifact.to_string(),
                    session: session.to_string(),
                })
                .await?;
            if reviews.is_empty() {
                println!("(no reviews)");
                return Ok(());
            }
            for review in reviews {
                let stale = if review.outdated { " stale" } else { "" };
                println!(
                    "#{} {} · draft rev {} · {} comments · {}{}",
                    review.id,
                    review.status,
                    review.draft_revision,
                    review.comments.len(),
                    review.delivery_state,
                    stale
                );
                for comment in review.comments {
                    println!(
                        "  {}  rev {}  {}",
                        comment.id,
                        comment.subject_version,
                        comment.body.replace('\n', " ")
                    );
                }
            }
            Ok(())
        }
        ReviewCmd::Show { review_id } => {
            let review = client
                .invoke::<reviews::get::Op>(&reviews::get::Input { id: review_id })
                .await?;
            println!("{}", serde_json::to_string_pretty(&review)?);
            Ok(())
        }
        ReviewCmd::Add {
            session,
            artifact,
            rev,
            quote,
            prefix,
            suffix,
            block,
            body,
        } => {
            let body = body.join(" ").trim().to_string();
            if body.is_empty() {
                bail!("a comment body is required");
            }
            let draft = client
                .invoke::<reviews::create::Op>(&reviews::create::Input {
                    session: session.to_string(),
                    subject_kind: ReviewSubjectKindDto::Artifact,
                    subject_key: artifact.clone(),
                    subject_version: rev.to_string(),
                })
                .await?;
            let comment = client
                .invoke::<reviews::comments::create::Op>(&reviews::comments::create::Input {
                    id: draft.id,
                    expected_revision: draft.draft_revision,
                    subject_version: rev.to_string(),
                    anchor_kind: ReviewAnchorKindDto::Text,
                    anchor: (ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                        quote,
                        prefix,
                        suffix,
                        block_index: block,
                    }))
                    .clone(),
                    body: body.clone(),
                })
                .await?;
            let comment_id = comment
                .comments
                .last()
                .map(|comment| comment.id)
                .ok_or_else(|| anyhow!("server returned a draft without the added comment"))?;
            println!(
                "draft review #{} · revision {} · comment {}",
                draft.id, comment.draft_revision, comment_id
            );
            Ok(())
        }
        ReviewCmd::Edit {
            review_id,
            comment_id,
            revision,
            body,
        } => {
            let body = body.join(" ").trim().to_string();
            if body.is_empty() {
                bail!("a comment body is required");
            }
            let comment = client
                .invoke::<reviews::comments::update::Op>(&reviews::comments::update::Input {
                    id: review_id,
                    comment_id,
                    expected_revision: revision,
                    body: (Some(body)).clone(),
                    ..Default::default()
                })
                .await?;
            println!(
                "updated comment {comment_id} · draft revision {}",
                comment.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Reanchor {
            review_id,
            comment_id,
            revision,
            rev,
            quote,
            prefix,
            suffix,
            block,
        } => {
            let comment = client
                .invoke::<reviews::comments::update::Op>(&reviews::comments::update::Input {
                    id: review_id,
                    comment_id,
                    expected_revision: revision,
                    body: None.clone(),
                    subject_version: (Some(rev.to_string())).clone(),
                    anchor_kind: (Some(ReviewAnchorKindDto::Text)),
                    anchor: (Some(ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                        quote,
                        prefix,
                        suffix,
                        block_index: block,
                    })))
                    .clone(),
                })
                .await?;
            println!(
                "re-anchored comment {comment_id} to revision {rev} · draft revision {}",
                comment.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Overall {
            session,
            artifact,
            rev,
            body,
        } => {
            let summary = body.join(" ").trim().to_string();
            if summary.is_empty() {
                bail!("an overall note is required");
            }
            let draft = client
                .invoke::<reviews::create::Op>(&reviews::create::Input {
                    session: session.to_string(),
                    subject_kind: ReviewSubjectKindDto::Artifact,
                    subject_key: artifact.clone(),
                    subject_version: rev.to_string(),
                })
                .await?;
            let draft = client
                .invoke::<reviews::update::Op>(&reviews::update::Input {
                    id: draft.id,
                    expected_revision: draft.draft_revision,
                    summary: (Some(summary)).clone(),
                    subject_version: None.clone(),
                })
                .await?;
            println!(
                "draft review #{} · revision {} · overall note saved",
                draft.id, draft.draft_revision
            );
            Ok(())
        }
        ReviewCmd::DeleteComment {
            review_id,
            comment_id,
            revision,
        } => {
            let review = client
                .invoke::<reviews::comments::delete::Op>(&reviews::comments::delete::Input {
                    id: review_id,
                    comment_id,
                    expected_revision: revision,
                })
                .await?;
            println!(
                "deleted comment {comment_id} · draft revision {}",
                review.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Resolve {
            review_id,
            comment_id,
        } => {
            let comment = client
                .invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
                    id: review_id,
                    comment_id,
                    resolved: true,
                })
                .await?;
            println!("resolved comment {}", comment.id);
            Ok(())
        }
        ReviewCmd::Reopen {
            review_id,
            comment_id,
        } => {
            let comment = client
                .invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
                    id: review_id,
                    comment_id,
                    resolved: false,
                })
                .await?;
            println!("reopened comment {}", comment.id);
            Ok(())
        }
        ReviewCmd::Discard {
            review_id,
            revision,
        } => {
            client.discard_review(review_id, revision).await?;
            println!("discarded review {review_id}");
            Ok(())
        }
        ReviewCmd::Retarget {
            review_id,
            revision,
        } => {
            let review = client
                .invoke::<reviews::retarget::Op>(&reviews::retarget::Input {
                    id: review_id,
                    expected_revision: revision,
                })
                .await?;
            println!(
                "review {} targets artifact revision {} · draft revision {}",
                review.id, review.subject.version, review.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Submit {
            review_id,
            revision,
            summary,
            acknowledge_outdated,
        } => {
            let revision = if summary.is_empty() {
                revision
            } else {
                client
                    .invoke::<reviews::update::Op>(&reviews::update::Input {
                        id: review_id,
                        expected_revision: revision,
                        summary: (Some(summary)).clone(),
                        subject_version: None.clone(),
                    })
                    .await?
                    .draft_revision
            };
            let review = client
                .invoke::<reviews::submit::Op>(&reviews::submit::Input {
                    id: review_id,
                    expected_revision: revision,
                    acknowledge_outdated,
                })
                .await?;
            println!(
                "submitted review {} · delivery {}",
                review.id, review.delivery_state
            );
            Ok(())
        }
        ReviewCmd::Retry { review_id } => {
            let review = client
                .invoke::<reviews::retry_delivery::Op>(&reviews::retry_delivery::Input {
                    id: review_id,
                })
                .await?;
            println!("review {} · delivery {}", review.id, review.delivery_state);
            Ok(())
        }
    }
}

/// Dispatch the `loom sessions <verb>` subcommands.
async fn run_session(cmd: SessionCmd) -> Result<()> {
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
            loom::agent_cli::run_tag(cmd).await
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
                loom::agent_cli::run_events(limit).await
            }
        }
        SessionCmd::Transcript { file, json } => loom::agent_cli::run_chatlog(file, json),
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

fn github_access_session(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| std::env::var("LOOM_SESSION_ID").ok())
        .or_else(|| std::env::var("WEAVER_BRANCH").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .context("not inside a loom session — pass the target explicitly with --session <session>")
}

async fn layout_revision(client: &Client, requested: Option<i64>) -> Result<i64> {
    match requested {
        Some(revision) => Ok(revision),
        None => Ok(client
            .invoke::<session_layout::get::Op>(&session_layout::get::Input {})
            .await?
            .revision),
    }
}

fn print_session_layout(layout: &SessionLayoutView) {
    println!("session layout revision {}", layout.revision);
    for space in &layout.spaces {
        println!("{}  {}  (rank {})", space.id, space.name, space.rank);
        for group in &space.groups {
            println!(
                "  {}  {}  (rank {}, {})",
                group.id,
                group.name,
                group.rank,
                if group.collapsed {
                    "collapsed"
                } else {
                    "expanded"
                }
            );
            for session_id in &group.session_ids {
                println!("    {session_id}");
            }
        }
    }
    if !layout.defaults.is_empty() {
        println!("defaults:");
        for default in &layout.defaults {
            println!(
                "  {}:{} -> {}",
                default.selector_kind, default.selector_value, default.group_id
            );
        }
    }
}

async fn run_session_layout(cmd: SessionLayoutCmd) -> Result<()> {
    let client = client::default()?;
    let layout = match cmd {
        SessionLayoutCmd::Show => {
            client
                .invoke::<session_layout::get::Op>(&session_layout::get::Input {})
                .await?
        }
        SessionLayoutCmd::SpaceAdd { name, revision } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::spaces::create::Op>(
                    &session_layout::spaces::create::Input {
                        name,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::SpaceRename { id, name, revision } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::spaces::update::Op>(
                    &session_layout::spaces::update::Input {
                        id,
                        name,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::SpaceDelete { id, to, revision } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::spaces::delete::Op>(
                    &session_layout::spaces::delete::Input {
                        id,
                        destination_group_id: to,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::GroupAdd {
            space,
            name,
            revision,
        } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::groups::create::Op>(
                    &session_layout::groups::create::Input {
                        space_id: space,
                        name,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::GroupRename { id, name, revision } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::groups::update::Op>(
                    &session_layout::groups::update::Input {
                        id,
                        name,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::GroupDelete { id, to, revision } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::groups::delete::Op>(
                    &session_layout::groups::delete::Input {
                        id,
                        destination_group_id: to,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::Reorder {
            kind,
            id,
            before,
            space,
            revision,
        } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::reorder::Op>(&session_layout::reorder::Input {
                    kind,
                    id,
                    before_id: before,
                    destination_space_id: space,
                    expected_revision,
                })
                .await?
        }
        SessionLayoutCmd::Move {
            to,
            before,
            revision,
            sessions,
        } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .move_sessions(&MoveSessionsReq {
                    session_ids: sessions,
                    destination_group_id: to,
                    before_session_id: before,
                    expected_revision,
                })
                .await?
        }
        SessionLayoutCmd::Restore { snapshot, revision } => {
            let groups = serde_json::from_str(&snapshot)
                .context("restore snapshot must be a JSON array of group orders")?;
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::restore::Op>(&session_layout::restore::Input {
                    groups,
                    expected_revision,
                })
                .await?
        }
        SessionLayoutCmd::Collapse { group } => {
            client
                .invoke::<session_layout::groups::preference::set::Op>(
                    &session_layout::groups::preference::set::Input {
                        id: group,
                        collapsed: true,
                    },
                )
                .await?
        }
        SessionLayoutCmd::Expand { group } => {
            client
                .invoke::<session_layout::groups::preference::set::Op>(
                    &session_layout::groups::preference::set::Input {
                        id: group,
                        collapsed: false,
                    },
                )
                .await?
        }
        SessionLayoutCmd::DefaultSet {
            kind,
            value,
            to,
            revision,
        } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::defaults::set::Op>(
                    &session_layout::defaults::set::Input {
                        selector_kind: kind,
                        selector_value: value,
                        group_id: to,
                        expected_revision,
                    },
                )
                .await?
        }
        SessionLayoutCmd::DefaultDelete {
            kind,
            value,
            revision,
        } => {
            let expected_revision = layout_revision(&client, revision).await?;
            client
                .invoke::<session_layout::defaults::delete::Op>(
                    &session_layout::defaults::delete::Input {
                        selector_kind: kind,
                        selector_value: value.to_string(),
                        expected_revision,
                    },
                )
                .await?
        }
    };
    print_session_layout(&layout);
    Ok(())
}

async fn run_mcp(cmd: McpCmd) -> Result<()> {
    match cmd {
        McpCmd::Serve { adapter } => loom::mcp::serve(&adapter).await,
        McpCmd::ServeCustom { identity } => loom::custom_mcp::serve_from_env(&identity).await,
        McpCmd::Ls => {
            let registry = client::default()?
                .invoke::<mcps::get::Op>(&mcps::get::Input {})
                .await?;
            for set in &registry.capability_sets {
                println!(
                    "{:<30} {:<4} {:<12} {}",
                    set.name, set.version, set.adapter, set.description
                );
            }
            for server in &registry.custom_servers {
                println!(
                    "{:<30} r{:<3} {:<12} {}",
                    server.identity, server.revision, server.validation_state, server.description
                );
            }
            Ok(())
        }
        McpCmd::Show { name } => {
            let registry = client::default()?
                .invoke::<mcps::get::Op>(&mcps::get::Input {})
                .await?;
            if let Some(server) = registry
                .custom_servers
                .iter()
                .find(|server| server.identity == name)
            {
                println!("{}", serde_json::to_string_pretty(server)?);
                return Ok(());
            }
            let set = registry
                .capability_sets
                .into_iter()
                .find(|set| set.name == name)
                .ok_or_else(|| anyhow!("unknown MCP capability set '{name}'"))?;
            println!("{}", serde_json::to_string_pretty(&set)?);
            Ok(())
        }
        McpCmd::Add(opts) => {
            let source = std::fs::read_to_string(&opts.file)
                .with_context(|| format!("reading custom MCP source {}", opts.file))?;
            let test_source = match &opts.tests {
                Some(path) => std::fs::read_to_string(path)
                    .with_context(|| format!("reading custom MCP tests {path}"))?,
                None => String::new(),
            };
            let req = weaver_api::CustomMcpReq {
                identity: opts.identity.clone(),
                label: opts.label.clone(),
                description: opts.description.clone(),
                source,
                test_source,
                enabled: !opts.disabled,
            };
            let registry = client::default()?
                .invoke::<mcps::get::Op>(&mcps::get::Input {})
                .await?;
            let value = if registry
                .custom_servers
                .iter()
                .any(|server| server.identity == opts.identity)
            {
                client::default()?
                    .invoke::<mcps::custom::update::Op>(&mcps::custom::update::Input {
                        identity: opts.identity.to_string(),
                        label: req.label.clone(),
                        description: req.description.clone(),
                        source: req.source.clone(),
                        test_source: req.test_source.clone(),
                        enabled: req.enabled,
                    })
                    .await?
            } else {
                client::default()?
                    .invoke::<mcps::custom::create::Op>(&mcps::custom::create::Input {
                        identity: req.identity.clone(),
                        label: req.label.clone(),
                        description: req.description.clone(),
                        source: req.source.clone(),
                        test_source: req.test_source.clone(),
                        enabled: req.enabled,
                    })
                    .await?
            };
            println!(
                "{} revision {} ({})",
                value.identity, value.revision, value.validation_state
            );
            if !value.validation_message.is_empty() {
                println!("{}", value.validation_message);
            }
            if value.validation_state != "ready" {
                bail!("custom MCP validation failed");
            }
            Ok(())
        }
        McpCmd::Rm { identity } => {
            client::default()?.delete_custom_mcp(&identity).await?;
            println!("removed {identity}");
            Ok(())
        }
    }
}

/// Dispatch the `loom watch <verb>` subcommands.
async fn run_watch(cmd: WatchCmd) -> Result<()> {
    match cmd {
        WatchCmd::New { name } => cmd_watch_new(name).await,
        WatchCmd::Programs { source } => cmd_watch_programs(source).await,
        WatchCmd::Add(opts) => cmd_watch_add(*opts).await,
        WatchCmd::Rm { name } => cmd_watch_rm(name).await,
        WatchCmd::Enable { name } => cmd_watch_set_enabled(name, true).await,
        WatchCmd::Disable { name } => cmd_watch_set_enabled(name, false).await,
        WatchCmd::Ls => cmd_watch_ls().await,
        WatchCmd::Run { name, dry_run } => cmd_watch_run(name, dry_run).await,
        WatchCmd::Runs { name, limit } => cmd_watch_runs(name, limit, false).await,
        WatchCmd::Logs { name, limit } => cmd_watch_runs(name, limit, true).await,
    }
}

async fn run_token(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::Add { name, expires_days } => cmd_token_create(name, expires_days).await,
        TokenCmd::Ls => cmd_token_ls().await,
        TokenCmd::Rm { id } => cmd_token_rm(id).await,
        TokenCmd::Mint {
            subject,
            profiles,
            ttl,
        } => {
            let minted = client::default()?
                .invoke::<auth::automation_token::Op>(&auth::automation_token::Input {
                    subject: subject.clone(),
                    profiles: profiles.clone(),
                    ttl_secs: (parse_ttl(&ttl)?),
                })
                .await?;
            println!("{}", minted.token);
            Ok(())
        }
    }
}

async fn cmd_login(name: String, url: Option<String>, token_stdin: bool) -> Result<()> {
    let paths = loom::client_context::ClientPaths::discover()?;
    let existing_url = loom::client_context::context_url(&paths, &name)?;
    let url = match url {
        Some(url) => url,
        None => prompt_line("Server URL", existing_url.as_deref())?,
    };
    let url = loom::client_context::normalize_url(&url)?;
    let token = if token_stdin {
        use std::io::Read as _;
        let mut token = String::new();
        std::io::stdin()
            .read_to_string(&mut token)
            .context("reading API token from stdin")?;
        token
    } else {
        rpassword::prompt_password("API token: ").context("reading API token")?
    };
    let token = token.trim();
    if token.is_empty() {
        bail!("API token must not be empty");
    }

    let remote = Client::new(url.clone()).with_token(Some(token.to_string()));
    let me = remote.post("/api/auth/me", json!({})).await?;
    if me.get("authenticated").and_then(Value::as_bool) != Some(true)
        || me.get("via").and_then(Value::as_str) != Some("token")
    {
        bail!("Loom rejected the personal API token");
    }
    remote
        .invoke::<auth::tokens::list::Op>(&auth::tokens::list::Input {})
        .await
        .context("credential is authenticated but is not a user API token")?;
    loom::client_context::save_login(&paths, &name, &url, token)?;
    let username = me
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("unknown user");
    println!("logged in to {url} as {username}");
    println!("current context: {name}");
    Ok(())
}

fn cmd_logout(name: String) -> Result<()> {
    let paths = loom::client_context::ClientPaths::discover()?;
    if loom::client_context::remove_login(&paths, &name)? {
        println!("removed saved credential for {name}");
    } else {
        println!("no saved credential for {name}");
    }
    Ok(())
}

fn run_client_context(cmd: ClientContextCmd) -> Result<()> {
    let paths = loom::client_context::ClientPaths::discover()?;
    match cmd {
        ClientContextCmd::Ls => {
            let contexts = loom::client_context::list_contexts(&paths)?;
            if contexts.is_empty() {
                println!("no contexts — add one with `loom context add <name> --url <url>`");
                return Ok(());
            }
            for context in contexts {
                let current = if context.is_default { "*" } else { " " };
                let auth = if context.authenticated {
                    "authenticated"
                } else {
                    "no credential"
                };
                println!("{current} {}  {}  {auth}", context.name, context.url);
            }
            Ok(())
        }
        ClientContextCmd::Use { name } => {
            loom::client_context::use_context(&paths, &name)?;
            println!("current context: {name}");
            Ok(())
        }
        ClientContextCmd::Add {
            name,
            url,
            use_context,
        } => {
            loom::client_context::save_context(&paths, &name, &url, use_context)?;
            println!("saved context {name}");
            Ok(())
        }
        ClientContextCmd::Current => {
            let selection = client::current_selection()?;
            match selection.source {
                client::ClientSelectionSource::Context { name, source } => {
                    let source_name = match source {
                        loom::client_context::ContextSource::Explicit => "--context",
                        loom::client_context::ContextSource::Environment => "LOOM_CONTEXT",
                        loom::client_context::ContextSource::Repository(path) => {
                            println!("selector: {}", path.display());
                            "repository"
                        }
                        loom::client_context::ContextSource::Default => "default",
                    };
                    println!("{name}  {}  {source_name}", selection.base);
                }
                client::ClientSelectionSource::Environment => {
                    println!("WEAVER_API  {}", selection.base)
                }
                client::ClientSelectionSource::Local => {
                    println!("local  {}  implicit", selection.base)
                }
            }
            Ok(())
        }
        ClientContextCmd::Rm { name } => {
            if loom::client_context::remove_context(&paths, &name)? {
                println!("removed context {name}");
            } else {
                println!("unknown context {name}");
            }
            Ok(())
        }
    }
}

fn parse_ttl(value: &str) -> Result<i64> {
    let value = value.trim();
    let (number, multiplier) = match value.chars().last() {
        Some('s') => (&value[..value.len() - 1], 1),
        Some('m') => (&value[..value.len() - 1], 60),
        Some('h') => (&value[..value.len() - 1], 3600),
        _ => (value, 1),
    };
    let amount: i64 = number.parse().context("invalid --ttl duration")?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("--ttl duration is too large"))
}

async fn run_federation(cmd: FederationCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        FederationCmd::Add(args) => {
            let FederationAddArgs {
                name,
                provider,
                issuer,
                audience,
                subject,
                service_account,
                service_tag,
                repository_id,
                workflow_ref,
                event,
                git_ref,
                profiles,
            } = *args;
            let name = name.unwrap_or_else(|| {
                use sha2::Digest as _;
                let identity = format!(
                    "{provider}:{}:{}:{}:{}",
                    subject.as_deref().unwrap_or_default(),
                    service_account.as_deref().unwrap_or_default(),
                    repository_id.as_deref().unwrap_or_default(),
                    workflow_ref.as_deref().unwrap_or_default(),
                );
                let digest = sha2::Sha256::digest(identity.as_bytes());
                format!("federation-{}", hex::encode(&digest[..8]))
            });
            let mapping = client
                .invoke::<auth::federations::create::Op>(&auth::federations::create::Input {
                    name: Some(name.clone()),
                    provider: provider.clone(),
                    issuer: issuer.clone(),
                    audience: audience.clone(),
                    subject: subject.clone(),
                    service_account: service_account.clone(),
                    service_tag: service_tag.clone(),
                    repository_id: repository_id.clone(),
                    workflow_ref: workflow_ref.clone(),
                    event_name: event.clone(),
                    ref_pattern: git_ref.clone(),
                    profiles: profiles.clone(),
                })
                .await?;
            println!("added federation mapping {}", mapping.id);
        }
        FederationCmd::Ls => {
            for mapping in client
                .invoke::<auth::federations::list::Op>(&auth::federations::list::Input {})
                .await?
            {
                println!(
                    "{}  provider={}  service={}  profiles={}",
                    mapping.name,
                    mapping.provider,
                    mapping.service_tag,
                    mapping.profiles.join(",")
                );
            }
        }
        FederationCmd::Rm { id } => {
            client.remove_federation(&id).await?;
            println!("removed federation mapping {id}");
        }
    }
    Ok(())
}

async fn run_deployment(cmd: DeploymentCmd) -> Result<()> {
    match cmd {
        DeploymentCmd::Apply { file } => {
            let contents = if file == "-" {
                use std::io::Read as _;
                let mut contents = String::new();
                std::io::stdin()
                    .read_to_string(&mut contents)
                    .context("reading deployment manifest from stdin")?;
                contents
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading deployment manifest {file}"))?
            };
            let request = parse_deployment_manifest(&contents)?;
            let result = client::default()?
                .invoke::<deployment::reconcile::Op>(&request)
                .await?;
            println!(
                "reconciled {} settings, {} profiles, and {} federation mappings",
                result.settings.len(),
                result.profiles.len(),
                result.federations.len()
            );
        }
    }
    Ok(())
}

fn parse_deployment_manifest(contents: &str) -> Result<deployment::reconcile::Input> {
    serde_yaml_ng::from_str(contents).context("decoding deployment manifest as YAML or JSON")
}

async fn run_profile(cmd: ProfileCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        ProfileCmd::Add(opts) => {
            let instructions = match opts.instructions_file.as_deref() {
                Some(path) => std::fs::read_to_string(path)
                    .with_context(|| format!("reading profile instructions {path}"))?,
                None => String::new(),
            };
            let profile = client
                .invoke::<profiles::create::Op>(&profiles::create::Input {
                    name: opts.name.clone(),
                    description: opts.description.clone(),
                    agent_kind: opts.agent.clone(),
                    model: opts.model.clone(),
                    effort: opts.effort.clone(),
                    protocol: opts.protocol.clone(),
                    mode: opts.mode.clone(),
                    class: opts.class.clone(),
                    strict: opts.strict,
                    env_clear: opts.env_clear,
                    ambient_allowlist: opts.ambient.clone(),
                    idle_archive_secs: opts.idle_archive_secs,
                    max_concurrent: opts.max_concurrent,
                    turn_budget: opts.turn_budget,
                    prelude: opts.prelude.clone(),
                    instructions: instructions.clone(),
                    restricted: opts.restricted,
                    github_repositories: (Vec::new()).clone(),
                    runtime_permissions: opts.runtime_permission.clone(),
                    mcp_access: (parse_mcp_access(&opts.mcp)?).clone(),
                })
                .await?;
            println!(
                "added profile {} (revision {})",
                profile.name, profile.revision
            );
        }
        ProfileCmd::Ls => {
            for profile in client
                .invoke::<profiles::list::Op>(&profiles::list::Input {})
                .await?
            {
                println!(
                    "{:<20} {:<11} {:<10} {:<8} {}",
                    profile.name,
                    profile.class,
                    profile.agent_kind,
                    if profile.strict { "strict" } else { "mutable" },
                    profile.description
                );
            }
        }
        ProfileCmd::Show { name, effective } => {
            println!(
                "{}",
                if effective {
                    serde_json::to_string_pretty(
                        &client
                            .invoke::<profiles::effective::Op>(&profiles::effective::Input {
                                name: name.to_string(),
                            })
                            .await?,
                    )?
                } else {
                    serde_json::to_string_pretty(
                        &client
                            .invoke::<profiles::get::Op>(&profiles::get::Input {
                                name: name.to_string(),
                            })
                            .await?,
                    )?
                }
            );
        }
        ProfileCmd::Resolve {
            name,
            agent,
            model,
            effort,
            protocol,
            mode,
            class,
        } => {
            let resolved = client
                .invoke::<sessions::launches::resolve::Op>(&sessions::launches::resolve::Input {
                    selection: (weaver_api::LaunchSelection {
                        profile: name,
                        overrides: weaver_api::LaunchOverrides {
                            agent,
                            model,
                            effort,
                            protocol,
                            mode,
                            class,
                        },
                    })
                    .clone(),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&resolved)?);
            if !resolved.valid {
                bail!("resolved launch is not currently valid");
            }
        }
        ProfileCmd::Clone {
            source,
            name,
            agent,
            model,
            effort,
            protocol,
            mode,
            class,
            copy_environment,
            remove_environment,
            set_environment,
            secret_environment,
        } => {
            let overrides = weaver_api::LaunchOverrides {
                agent,
                model,
                effort,
                protocol,
                mode,
                class,
            };
            let resolved = client
                .invoke::<sessions::launches::resolve::Op>(&sessions::launches::resolve::Input {
                    selection: (weaver_api::LaunchSelection {
                        profile: source.clone(),
                        overrides: overrides.clone(),
                    })
                    .clone(),
                })
                .await?;
            let parse_environment = |raw: String, secret: bool| -> anyhow::Result<_> {
                let (name, value) = raw
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("environment edits must use NAME=VALUE"))?;
                if name.trim().is_empty() {
                    bail!("environment name must not be empty");
                }
                Ok(weaver_api::ProfileEnvMutationReq {
                    name: name.to_string(),
                    value: (!secret).then(|| value.to_string()),
                    secret_ref: secret.then(|| value.to_string()),
                })
            };
            let mut environment_set = Vec::new();
            for raw in set_environment {
                environment_set.push(parse_environment(raw, false)?);
            }
            for raw in secret_environment {
                environment_set.push(parse_environment(raw, true)?);
            }
            let has_environment_proposal =
                copy_environment || !remove_environment.is_empty() || !environment_set.is_empty();
            let saved = client
                .invoke::<profiles::clone::Op>(&profiles::clone::Input {
                    source: source.to_string(),
                    name: name.clone(),
                    expected_profile_revision: resolved.profile_revision,
                    expected_resolver_revision: resolved.resolver_revision.clone(),
                    overrides: overrides.clone(),
                    template: None.clone(),
                    copy_environment,
                    environment: (has_environment_proposal.then_some(
                        weaver_api::CloneProfileEnvironmentReq {
                            inherit: copy_environment,
                            remove: remove_environment,
                            set: environment_set,
                        },
                    ))
                    .clone(),
                })
                .await?;
            println!(
                "cloned {source} as {} (revision {})",
                saved.name, saved.revision
            );
        }
        ProfileCmd::Rm { name } => {
            client.delete_profile(&name).await?;
            println!("removed profile {name}");
        }
        ProfileCmd::Env { cmd } => match cmd {
            ProfileEnvCmd::Set {
                profile,
                name,
                value,
            } => {
                client
                    .invoke::<profiles::env::set::Op>(&profiles::env::set::Input {
                        profile: profile.to_string(),
                        name: name.to_string(),
                        value: Some(value.to_string()),
                        secret_ref: None,
                    })
                    .await?;
                println!("set {name} on profile {profile}");
            }
            ProfileEnvCmd::Secret {
                profile,
                name,
                secret_ref,
            } => {
                client
                    .invoke::<profiles::env::set::Op>(&profiles::env::set::Input {
                        profile: profile.to_string(),
                        name: name.to_string(),
                        value: None,
                        secret_ref: Some(secret_ref.to_string()),
                    })
                    .await?;
                println!("set Secret Manager reference for {name} on profile {profile}");
            }
            ProfileEnvCmd::Rm { profile, name } => {
                client
                    .invoke::<profiles::env::delete::Op>(&profiles::env::delete::Input {
                        profile: profile.to_string(),
                        name: name.to_string(),
                    })
                    .await?;
                println!("removed {name} from profile {profile}");
            }
        },
    }
    Ok(())
}

fn parse_mcp_access(value: &str) -> Result<weaver_api::McpAccess> {
    let value = value.trim();
    if matches!(value, "none" | "all") {
        return Ok(weaver_api::McpAccess {
            mode: value.to_string(),
            groups: Vec::new(),
        });
    }
    let groups = value
        .split(',')
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if groups.is_empty() {
        bail!("--mcp must be 'none', 'all', or a comma-separated group list");
    }
    Ok(weaver_api::McpAccess {
        mode: "groups".to_string(),
        groups,
    })
}

async fn cmd_token_create(name: String, expires_days: Option<i64>) -> Result<()> {
    let created = client::default()?
        .invoke::<auth::tokens::create::Op>(&auth::tokens::create::Input {
            name: name.clone(),
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
    let tokens = client::default()?
        .invoke::<auth::tokens::list::Op>(&auth::tokens::list::Input {})
        .await?;
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
    client::default()?.revoke_token(&id).await?;
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
        if loom::github_trigger::valid_login(&login) {
            break login;
        }
        println!("  '{login}' isn't a valid GitHub login (letters, digits, and hyphens only).");
    };
    if loom::auth::get_user(&db, &owner).await?.is_none() {
        loom::auth::add_user(&db, &owner, Some(&owner), None, loom::auth::UserRole::Admin)
            .await
            .with_context(|| format!("seeding the bootstrap operator '{owner}'"))?;
    }
    loom::loom_config::upsert(&config_path, &[("LOOM_OWNER_GITHUB", owner.as_str())])
        .context("writing the operator into loom.toml")?;
    println!("  ✓ '{owner}' can sign in and trigger sessions by commenting.");
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
/// stored on a fresh or incompletely configured instance.
async fn existing_app(db: &Db) -> Option<ExistingApp> {
    let nonempty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let id = nonempty(weaver_core::config::get(db, loom::github_app::APP_ID_KEY).await)?;
    Some(ExistingApp {
        id,
        slug: nonempty(weaver_core::config::get(db, loom::github_app::APP_SLUG_KEY).await),
        org: nonempty(weaver_core::config::get(db, loom::github_app::APP_OWNER_KEY).await),
    })
}

/// Present the update / re-install menu for an already-configured App. Returns
/// `true` when the operator chose to create a brand-new App; `false` when the
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
/// Loom's sqlite database directly through its daemon-less setup path;
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
    // out (see `db::seed_owner`). Resolve before opening the callback listener
    // so configuration errors fail before the browser confirmation.
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

    weaver_core::config::apply(
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

    // Approve that individual so they can sign in and trigger sessions. Written
    // live to the running daemon here, and to loom.toml (`LOOM_OWNER_GITHUB`)
    // below for a fresh DB. Their triggers on any repo the App is installed on
    // auto-register it — so an org install needs no separate owner allowlist.
    // Add more people in Settings → People & security.
    if loom::auth::get_user(&db, owner_login).await?.is_none() {
        loom::auth::add_user(
            &db,
            owner_login,
            Some(owner_login),
            None,
            loom::auth::UserRole::Admin,
        )
        .await
        .context("approving the bootstrap operator")?;
    }
    println!(
        "Approved '{owner_login}' — they can sign in and trigger sessions. Add more in \
         Settings → People & security."
    );

    let updates: Vec<(&str, &str)> = vec![
        ("LOOM_GITHUB_APP_ID", app_id.as_str()),
        ("LOOM_GITHUB_APP_SLUG", conv.slug.as_str()),
        ("LOOM_GITHUB_APP_PRIVATE_KEY", conv.pem.as_str()),
        ("LOOM_GITHUB_WEBHOOK_SECRET", conv.webhook_secret.as_str()),
        ("LOOM_GITHUB_CLIENT_ID", conv.client_id.as_str()),
        ("LOOM_GITHUB_CLIENT_SECRET", conv.client_secret.as_str()),
        ("LOOM_DOMAIN", domain),
        ("LOOM_OWNER_GITHUB", owner_login),
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

    println!("Paste-once secrets for the default profile (leave blank to skip).");
    let anthropic = prompt_secret(
        "ANTHROPIC_API_KEY",
        existing_names.contains("ANTHROPIC_API_KEY"),
    )?;
    let openai = prompt_secret("OPENAI_API_KEY", existing_names.contains("OPENAI_API_KEY"))?;

    if anthropic.is_none() && openai.is_none() {
        println!("nothing entered — existing values kept unchanged");
        return Ok(());
    }

    let mut stored = Vec::new();
    if let Some(v) = &anthropic {
        loom::agent_env::set(&db, "ANTHROPIC_API_KEY", v).await?;
        stored.push("ANTHROPIC_API_KEY");
    }
    if let Some(v) = &openai {
        loom::agent_env::set(&db, "OPENAI_API_KEY", v).await?;
        stored.push("OPENAI_API_KEY");
    }
    println!();
    println!(
        "Stored {} on the default profile — future sessions using that profile get them \
         (Settings → Agents & profiles in the web UI, or `loom settings env list`).",
        stored.join(", ")
    );
    let mut updates: Vec<(&str, &str)> = Vec::new();
    if let Some(v) = &anthropic {
        updates.push(("ANTHROPIC_API_KEY", v.as_str()));
    }
    if let Some(v) = &openai {
        updates.push(("OPENAI_API_KEY", v.as_str()));
    }
    loom::loom_config::upsert(&opts.config.config, &updates)
        .context("writing the paste-once secrets into loom.toml")?;
    println!(
        "Also wrote them to {} — run `loom config render-env` then restart the daemon (e.g. \
         `docker compose up -d`) to apply them.",
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
        if loom::github_trigger::valid_login(&login) {
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
// shared contract deployment tooling builds against; see `Cmd::Config`.
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
/// the settings pane's `settings.patch` against a running daemon — the
/// form a deploy's boot sequence needs, since it must seed the auth settings
/// *before* loom starts listening.
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
/// when an `ANTHROPIC_API_KEY` or similar setting happens to be exported
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
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("loom=info,weaver_core=info,tower_http=warn"));
    // Registry-of-layers so the ring-buffer capture (the in-browser log viewer)
    // runs *alongside* the existing stdout output — `docker compose logs` is
    // unchanged; the buffer just tees. The one `EnvFilter` gates both layers.
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(loom::logs::layer())
        .init();
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
fn launch_underspecified(a: &LaunchArgs) -> bool {
    a.goal.trim().is_empty()
        && a.name.is_none()
        && a.title.is_none()
        && a.issue.is_none()
        && a.claim.is_none()
        && a.branch.is_none()
}

const LAUNCH_HINT: &str = "nothing to do — give the agent a task or something to pick up:
  loom sessions launch \"<what the agent should do>\"  # the common case
  loom sessions launch --claim <id>                    # pick up a Loom issue
  loom sessions launch --issue <n>                     # seed from a GitHub issue
  loom sessions launch --branch <name>                 # resume an existing branch
  loom sessions launch --name <slug> --agent shell     # an empty named worktree (no task)
See `loom sessions launch --help` for all options.";

/// What a launch forks from, once `--repo` has been classified.
#[derive(Debug, PartialEq, Eq)]
enum RepoTarget {
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
fn resolve_repo_target(repo: Option<&str>) -> Result<RepoTarget> {
    let Some(input) = repo.map(str::trim).filter(|s| !s.is_empty()) else {
        let cwd = std::env::current_dir().context("could not read the current directory")?;
        return Ok(RepoTarget::Local(cwd));
    };
    // Canonicalizing anchors a relative path to the CLI's cwd, not the daemon's.
    if let Ok(path) = std::path::Path::new(input).canonicalize() {
        return Ok(RepoTarget::Local(path));
    }
    if loom::repo::parse_slug(input).is_ok() {
        return Ok(RepoTarget::Managed(input.to_string()));
    }
    bail!(
        "--repo '{input}' is neither a local path that exists nor a repo to clone \
         (expected `owner/name` or a clone URL)"
    )
}

async fn cmd_launch(a: LaunchArgs) -> Result<()> {
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
async fn fetch_session(client: &Client, key: &str) -> Result<Value> {
    client
        .post("/api/sessions/get", json!({ "session": key }))
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

/// `loom sessions url` — print a session's dashboard URL, defaulting to the
/// session we are running inside. The server resolves the URL (only it knows
/// loom's public origin); this just prints it bare, so it composes into a
/// `gh pr create --body "$(…)"` without any trimming.
async fn cmd_session_url(key: Option<String>) -> Result<()> {
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
    let res: Value = client
        .post("/api/sessions/url", json!({ "session": key }))
        .await
        .with_context(|| format!("no live session for '{key}'"))?;
    let url = res
        .get("url")
        .and_then(Value::as_str)
        .context("server returned no url")?;
    println!("{url}");
    Ok(())
}

/// `loom sessions poll` — a one-shot status read: lifecycle + attention.
async fn cmd_session_poll(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = fetch_session(&client, &key).await?;
    println!(
        "session {}  ({})",
        str_field(&ws, "id"),
        branch_str(&ws, "name")
    );
    println!("  status:    {}", str_field(&ws, "status"));
    println!("  attention: {}", attention_summary(&ws));
    println!("  channel:   {}", str_field(&ws, "id"));
    if let Some(n) = ws.get("tracking_issue").and_then(Value::as_i64) {
        println!("  track:     Loom issue #{n}");
    }
    println!("  activity:  {}", str_field(&ws, "last_activity_at"));
    Ok(())
}

/// `loom sessions wait` — block until the session finishes, is lost, or (unless
/// `lifecycle_only`) its agent raises attention.
async fn cmd_session_wait(
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
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "done" | "error" | "archived")
}

/// `loom sessions send` — type a message into the agent's pane, submitting it
/// (Enter) unless `submit` is false.
async fn cmd_session_send(key: String, message: String, submit: bool) -> Result<()> {
    if message.trim().is_empty() {
        bail!("nothing to send — provide a message");
    }
    let client = client::default()?;
    client
        .post(
            "/api/sessions/send",
            json!({ "session": key, "text": message, "submit": submit }),
        )
        .await?;
    println!(
        "sent to {key}{}",
        if submit { "" } else { " (not submitted)" }
    );
    Ok(())
}

/// `loom sessions interrupt` — interrupt the agent's current turn.
async fn cmd_session_interrupt(key: String) -> Result<()> {
    let client = client::default()?;
    client
        .post("/api/sessions/interrupt", json!({ "session": key }))
        .await?;
    println!("interrupted {key}");
    Ok(())
}

/// `loom sessions preview` — print the session's recent terminal screen.
async fn cmd_session_preview(key: String, lines: usize) -> Result<()> {
    let client = client::default()?;
    let res = client
        .post(
            "/api/sessions/preview",
            json!({ "session": key, "lines": lines }),
        )
        .await?;
    print!("{}", str_field(&res, "screen"));
    // The capture is right-trimmed server-side; ensure a clean final newline.
    println!();
    Ok(())
}

#[derive(Default)]
struct PsOptions {
    archived: bool,
    managed: bool,
    search: Option<String>,
    status: Option<SessionSearchStatus>,
    attention: Option<SessionSearchAttention>,
    creator: Option<SessionCreatorFilter>,
}

async fn cmd_ps(options: PsOptions) -> Result<()> {
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
    let list = serde_json::to_value(
        client
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
            .await?,
    )?;
    let rows = list.as_array().cloned().unwrap_or_default();
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
    for ws in rows {
        let location = ws.get("placement").map_or_else(String::new, |placement| {
            format!(
                "{}/{}",
                str_field(placement, "space_name"),
                str_field(placement, "group_name")
            )
        });
        println!(
            "{:<10}  {:<9}  {:<10}  {:<22}  {:<24}  {}",
            str_field(&ws, "id"),
            str_field(&ws, "status"),
            branch_attention(&ws),
            truncate(branch_str(&ws, "name"), 22),
            truncate(&location, 24),
            truncate(branch_str(&ws, "title"), 46),
        );
    }
    Ok(())
}

async fn cmd_show(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .post("/api/sessions/get", json!({ "session": key }))
        .await?;
    print_session(&ws);
    Ok(())
}

/// `loom sessions rename` — set a session's one-line dashboard title
/// (`sessions.update`). This keeps the CLI at parity with the dashboard's inline
/// title editor: the observed label and provenance travel with the edit so a
/// concurrent rename is rejected rather than silently overwritten.
async fn cmd_session_rename(key: String, title: String) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!("nothing to rename to — provide a new title");
    }
    let client = client::default()?;
    let current = client
        .post("/api/sessions/get", json!({ "session": key }))
        .await?;
    let ws = client
        .post(
            "/api/sessions/update",
            json!({
                "session": key,
                "title": title,
                "expected_title": branch_str(&current, "title"),
                "expected_title_provenance": branch_str(&current, "title_provenance"),
            }),
        )
        .await?;
    println!(
        "renamed {} → {}",
        str_field(&ws, "id"),
        branch_str(&ws, "title")
    );
    Ok(())
}

async fn cmd_session_regenerate_title(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .post("/api/sessions/title/regenerate", json!({ "session": key }))
        .await?;
    println!(
        "{} — {}",
        branch_str(&ws, "title"),
        str_field(ws.get("title_generation").unwrap_or(&Value::Null), "status")
    );
    Ok(())
}

async fn cmd_session_title_generation(key: String, enabled: bool) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .post(
            "/api/sessions/title/generation/set",
            json!({ "session": key, "enabled": enabled }),
        )
        .await?;
    println!(
        "title generation {} ({})",
        if enabled { "enabled" } else { "disabled" },
        str_field(ws.get("title_generation").unwrap_or(&Value::Null), "status")
    );
    Ok(())
}

/// How long `session cue --ensure` follows a generation it started. Covers the
/// server's own 45s prompt timeout with room for the runtime to spawn first.
const CUE_POLL_ATTEMPTS: usize = 40;
const CUE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

async fn cmd_session_cue(key: String, ensure: bool, force: bool) -> Result<()> {
    let client = client::default()?;
    let mut cue = if ensure {
        client
            .post(
                "/api/sessions/resumption_cue/ensure",
                json!({ "session": key, "force": force }),
            )
            .await?
    } else {
        client
            .post(
                "/api/sessions/resumption_cue/get",
                json!({ "session": key }),
            )
            .await?
    };
    // An ensure only *starts* generation — the model call runs detached so it
    // cannot hold a connection open. Wait it out here so the command still
    // prints a cue rather than the status of a request that just left.
    if ensure {
        for _ in 0..CUE_POLL_ATTEMPTS {
            if str_field(&cue, "status") != "generating" {
                break;
            }
            tokio::time::sleep(CUE_POLL_INTERVAL).await;
            cue = client
                .post(
                    "/api/sessions/resumption_cue/get",
                    json!({ "session": key }),
                )
                .await?;
        }
    }
    println!("status: {}", str_field(&cue, "status"));
    if let Some(text) = cue.get("text").and_then(Value::as_str) {
        println!("{text}");
    }
    if let Some(at) = cue.get("generated_at").and_then(Value::as_str) {
        println!("generated: {at}");
    }
    Ok(())
}

fn print_session(ws: &Value) {
    println!(
        "session {}  ({})",
        str_field(ws, "id"),
        branch_str(ws, "name")
    );
    println!(
        "  title:    {} ({})",
        branch_str(ws, "title"),
        branch_str(ws, "title_provenance")
    );
    if let Some(generation) = ws.get("title_generation") {
        println!(
            "  title AI: {} ({})",
            if generation
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "enabled"
            } else {
                "disabled"
            },
            str_field(generation, "status")
        );
    }
    println!("  status:   {}", str_field(ws, "status"));
    if let Some(placement) = ws.get("placement").filter(|value| !value.is_null()) {
        println!(
            "  location: {} / {}",
            str_field(placement, "space_name"),
            str_field(placement, "group_name")
        );
    }
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
    let exact_parent = str_field(ws, "parent_session_id");
    if !exact_parent.is_empty() {
        println!("  parent:   session {exact_parent}");
    } else {
        let legacy_parent = str_field(ws, "parent_id");
        if !legacy_parent.is_empty() {
            println!("  parent:   branch {legacy_parent} (legacy)");
        }
    }
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
    let client = client::default()?;
    let ws = client
        .post("/api/sessions/get", json!({ "session": key }))
        .await?;
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
    let client = client::default()?;
    let res = client
        .post("/api/sessions/archive", json!({ "session": key }))
        .await?;
    if str_field(&res, "kind") == "launch_attempt" {
        println!("archived launch attempt {key} (reserved runtime removed; history kept)");
    } else {
        println!(
            "archived {} (terminal + worktree removed; branch and history kept)",
            str_field(&res, "branch")
        );
    }
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
    let client = client::default()?;
    let ws = client
        .post("/api/sessions/adopt", json!({ "session": key }))
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

async fn cmd_recover(key: String) -> Result<()> {
    let client = client::default()?;
    let ws = client
        .post("/api/sessions/recover", json!({ "session": key }))
        .await?;
    println!(
        "recovered session {}  ({})",
        str_field(&ws, "id"),
        branch_str(&ws, "name")
    );
    println!("  status:  {}", str_field(&ws, "status"));
    println!("  session: {}", str_field(&ws, "term_session"));
    println!("  attach:  loom attach {}", str_field(&ws, "id"));
    Ok(())
}

async fn cmd_handoff(
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

async fn cmd_rm(key: String, keep_branch: bool) -> Result<()> {
    let client = client::default()?;
    let res = client
        .post(
            "/api/sessions/delete",
            json!({ "session": key, "keep_branch": keep_branch }),
        )
        .await?;
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
    let client = client::default()?;
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
// Watch commands (the operator + authoring surface)
// ---------------------------------------------------------------------------

/// The starter program a `loom watch new` scaffolds: a small, runnable
/// template against the `weaver_loom` API layer and the program contract the
/// engine speaks — the same shape the builtin scripts implement
/// (`loom watch programs --source <name>` prints one as a fuller
/// example). Plain `replace` rather than `format!`, so the template's literal
/// braces (JSON, f-strings) stay readable.
fn scaffold_template(name: &str) -> String {
    const TEMPLATE: &str = r##"# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""__NAME__ — a weaver watch program.

The engine runs this as a subprocess with WEAVER_API (the loom REST base URL)
and WEAVER_WATCH (the round config JSON) set; `weaver_loom` is on
PYTHONPATH. `Round.finish` prints the result the engine reads from stdout.

Register:   loom watch add __NAME__ --program __PATH__ --every 15m
Try it:     loom watch run __NAME__ --dry-run
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
        .replace("__PATH__", &watch_path(name).display().to_string())
}

/// The conventional path for a watch's program file:
/// `~/.weaver/watches/<name>.py`.
fn watch_path(name: &str) -> std::path::PathBuf {
    loom::db::weaver_home()
        .join("watches")
        .join(format!("{name}.py"))
}

/// `loom watch new` — scaffold a starter program file and print its path.
/// A local file-convention command: it touches no server (T8 file convention),
/// so it works before the Python binding exists.
async fn cmd_watch_new(name: String) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("name must not be empty");
    }
    let dir = loom::db::weaver_home().join("watches");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = watch_path(name);
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
        "    loom watch add {name} --program {} --cron \"0 * * * *\"",
        path.display()
    );
    Ok(())
}

/// `loom watch programs` — list the builtin programs that ship with loom
/// (the registry the panel offers), or print one program's script source with
/// `--source` as a working example to start a custom program from.
async fn cmd_watch_programs(source: Option<String>) -> Result<()> {
    let client = client::default()?;
    let rows = client
        .post("/api/watches/programs", json!({}))
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(want) = source {
        let row = rows.iter().find(|p| str_field(p, "program") == want);
        let Some(row) = row else {
            bail!("no builtin program '{want}' — `loom watch programs` lists them");
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
/// `--repo` filter folded in so a repo-pinned watch only surveys its repo.
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

/// `loom watch add` — register a watch (`watches.create`).
async fn cmd_watch_add(opts: AddOpts) -> Result<()> {
    let client = client::default()?;
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
    if let Some(profile) = &opts.profile {
        body.insert("profile".into(), json!(profile));
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

    let o = client
        .post("/api/watches/create", Value::Object(body))
        .await?;
    println!(
        "registered watch {}  ({})",
        str_field(&o, "name"),
        str_field(&o, "id")
    );
    println!("  trigger: {}", trigger_summary(&o));
    println!("  program: {}", str_field(&o, "program"));
    println!("  caps:    {}", capabilities_summary(&o));
    println!("  profile: {}", str_field(&o, "profile"));
    println!(
        "  enabled: no — arm it with `loom watch enable {}`",
        opts.name
    );
    Ok(())
}

/// `loom watch rm` — delete a watch.
async fn cmd_watch_rm(name: String) -> Result<()> {
    let client = client::default()?;
    client
        .post("/api/watches/delete", json!({ "key": name }))
        .await?;
    println!("removed watch {name}");
    Ok(())
}

/// `loom watch enable|disable` — flip the `enabled` toggle
/// (`watches.update`).
async fn cmd_watch_set_enabled(name: String, enabled: bool) -> Result<()> {
    let client = client::default()?;
    let o = client
        .post(
            "/api/watches/update",
            json!({ "key": name, "enabled": enabled }),
        )
        .await?;
    println!(
        "{} watch {}",
        if enabled { "enabled" } else { "disabled" },
        str_field(&o, "name")
    );
    Ok(())
}

/// `loom watch ls` — a table of every watch.
async fn cmd_watch_ls() -> Result<()> {
    let client = client::default()?;
    let rows = client
        .post("/api/watches/list", json!({}))
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("no watches — scaffold one with `loom watch new <name>`");
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

/// `loom watch run` — fire a round now and print outcome + summary.
async fn cmd_watch_run(name: String, dry_run: bool) -> Result<()> {
    let client = client::default()?;
    let res = client
        .post(
            "/api/watches/run",
            json!({ "key": name, "dry_run": dry_run }),
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

/// `loom watch runs` / `logs` — the round history. `verbose` (the `logs`
/// alias) also prints each round's actions.
async fn cmd_watch_runs(name: String, limit: i64, verbose: bool) -> Result<()> {
    let client = client::default()?;
    let rows = client
        .post("/api/watches/runs", json!({ "key": name, "limit": limit }))
        .await?
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("no rounds yet for {name} — fire one with `loom watch run {name}`");
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

/// A compact human summary of an `WatchView`'s parsed `trigger` object.
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

/// The granted capability set, comma-joined, for an `WatchView`.
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

    #[test]
    fn a_session_may_not_start_a_server_on_the_host_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("loom.json"), "{}").unwrap();
        assert_eq!(
            nested_server_refusal(Some("bej3oxrv"), home.path()).unwrap(),
            format!(
                "refusing to start: this is Loom session bej3oxrv, and {} already belongs to a running loom. A second server on one home races the host's monitor, Slack client, and session teardown. Run `WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0` for an isolated instance.",
                home.path().display()
            )
        );
    }

    #[test]
    fn a_private_weaver_home_keeps_hand_testing_available() {
        // The session env always carries a WEAVER_HOME — the host's. What makes
        // an isolated home safe is that no loom lives in it yet.
        let home = tempfile::tempdir().unwrap();
        assert!(nested_server_refusal(Some("bej3oxrv"), home.path()).is_none());
    }

    #[test]
    fn the_host_server_is_not_a_session() {
        // The host's own restart finds its predecessor's loom.json and must
        // still start; only a session is refused.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("loom.json"), "{}").unwrap();
        assert!(nested_server_refusal(None, home.path()).is_none());
        assert!(nested_server_refusal(Some(""), home.path()).is_none());
    }

    use super::*;

    #[test]
    fn client_context_commands_parse() {
        let Cli { context, cmd } =
            Cli::try_parse_from(["loom", "--context", "local", "session", "ls"]).unwrap();
        assert_eq!(context.as_deref(), Some("local"));
        assert!(matches!(
            cmd,
            Cmd::Registered(RegisteredCliCommand::Sessions(_))
        ));

        let Cli { cmd, .. } = Cli::try_parse_from([
            "loom",
            "context",
            "add",
            "production",
            "--url",
            "https://loom.example.com",
            "--use",
        ])
        .unwrap();
        assert!(matches!(
            cmd,
            Cmd::Host(HostCmd::Context {
                cmd: ClientContextCmd::Add {
                    name,
                    use_context: true,
                    ..
                }
            }) if name == "production"
        ));
    }

    #[test]
    fn review_commands_parse_the_shared_rest_contract() {
        let Cli { cmd, .. } = Cli::try_parse_from([
            "loom",
            "review",
            "add",
            "session-1",
            "design",
            "--rev",
            "3",
            "--quote",
            "selected text",
            "--block",
            "7",
            "Tighten",
            "this",
            "claim.",
        ])
        .unwrap();
        assert!(matches!(
            cmd,
            Cmd::Registered(RegisteredCliCommand::Review(ReviewCmd::Add {
                    session,
                    artifact,
                    rev: 3,
                    block: Some(7),
                    body,
                    ..
                })) if session == "session-1"
                && artifact == "design"
                && body == ["Tighten", "this", "claim."]
        ));

        for args in [
            vec!["loom", "review", "ls", "session-1", "design"],
            vec!["loom", "review", "show", "4"],
            vec![
                "loom",
                "review",
                "edit",
                "4",
                "9",
                "--revision",
                "2",
                "new body",
            ],
            vec![
                "loom",
                "review",
                "reanchor",
                "4",
                "9",
                "--revision",
                "2",
                "--rev",
                "4",
                "--quote",
                "new text",
            ],
            vec![
                "loom",
                "review",
                "overall",
                "session-1",
                "design",
                "--rev",
                "3",
                "overall note",
            ],
            vec![
                "loom",
                "review",
                "delete-comment",
                "4",
                "9",
                "--revision",
                "2",
            ],
            vec!["loom", "review", "resolve", "4", "9"],
            vec!["loom", "review", "reopen", "4", "9"],
            vec!["loom", "review", "discard", "4", "--revision", "2"],
            vec!["loom", "review", "retarget", "4", "--revision", "2"],
            vec![
                "loom",
                "review",
                "submit",
                "4",
                "--revision",
                "2",
                "--acknowledge-outdated",
            ],
            vec!["loom", "review", "retry", "4"],
        ] {
            assert!(matches!(
                Cli::try_parse_from(args).unwrap().cmd,
                Cmd::Registered(RegisteredCliCommand::Review(_))
            ));
        }
    }

    #[test]
    fn profile_mcp_cli_parses_modes_and_groups() {
        assert_eq!(
            parse_mcp_access("none").unwrap(),
            weaver_api::McpAccess::default()
        );
        assert_eq!(
            parse_mcp_access("all").unwrap(),
            weaver_api::McpAccess {
                mode: "all".to_string(),
                groups: vec![],
            }
        );
        assert_eq!(
            parse_mcp_access("github, messaging").unwrap(),
            weaver_api::McpAccess {
                mode: "groups".to_string(),
                groups: vec!["github".to_string(), "messaging".to_string()],
            }
        );
        assert!(parse_mcp_access("").is_err());

        let Cli { cmd, .. } =
            Cli::try_parse_from(["loom", "mcp", "show", "mcp/github/comment@v1"]).unwrap();
        assert!(matches!(
            cmd,
            Cmd::Host(HostCmd::Mcp {
                cmd: McpCmd::Show { name }
            }) if name == "mcp/github/comment@v1"
        ));

        let Cli { cmd, .. } = Cli::try_parse_from([
            "loom",
            "profile",
            "add",
            "ops",
            "--agent",
            "claude",
            "--protocol",
            "acp",
            "--mcp",
            "github,messaging",
        ])
        .unwrap();
        assert!(matches!(
            cmd,
            Cmd::Host(HostCmd::Profile {
                cmd: ProfileCmd::Add(options)
            }) if options.mcp == "github,messaging"
        ));
    }

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

    /// clap's own consistency check over the full command tree — catches a
    /// malformed arg/subcommand (e.g. the nested `session` group) at test time
    /// rather than on first run.
    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
        let parsed =
            Cli::try_parse_from(["loom", "session", "title-generation", "task-1", "false"])
                .unwrap();
        assert!(matches!(
            parsed.cmd,
            Cmd::Registered(RegisteredCliCommand::Sessions(
                SessionCmd::TitleGeneration {
                    session,
                    enabled: false,
                }
            )) if session == "task-1"
        ));

        for verb in ["interrupt", "break"] {
            let parsed = Cli::try_parse_from(["loom", "session", verb, "task-1"]).unwrap();
            assert!(matches!(
                parsed.cmd,
                Cmd::Registered(RegisteredCliCommand::Sessions(
                    SessionCmd::Interrupt { session }
                )) if session == "task-1"
            ));
        }
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
        for s in ["created", "running", "orphaned"] {
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
            profile: None,
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
        assert!(out.contains("WEAVER_WATCH"));
        assert!(out.contains("from weaver_loom import Round"));
        assert!(out.contains("loom watch add test-watch"));
    }

    /// `loom watch new` writes the file under `~/.weaver/watches/`,
    /// creating the dir, and refuses to clobber an existing one.
    #[tokio::test]
    #[serial_test::serial]
    async fn watch_new_scaffolds_under_weaver_home() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        cmd_watch_new("scaffolded".to_string()).await.unwrap();
        let path = home.path().join("watches").join("scaffolded.py");
        assert!(path.exists(), "the program file was written");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("\"\"\"scaffolded — "));
        // A second `new` of the same name refuses rather than clobbering.
        assert!(cmd_watch_new("scaffolded".to_string()).await.is_err());
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
    fn deployment_manifest_accepts_yaml_and_json_scalars() {
        let yaml = parse_deployment_manifest(
            r#"
settings:
  slack.status_updates: false
  slack.idle_archive_secs: 7200
  slack.prompt_instructions: |
    Answer in the thread.
    Keep it concise.
prune: true
"#,
        )
        .unwrap();
        assert_eq!(yaml.settings["slack.status_updates"].stored(), "false");
        assert_eq!(yaml.settings["slack.idle_archive_secs"].stored(), "7200");
        assert_eq!(
            yaml.settings["slack.prompt_instructions"].stored(),
            "Answer in the thread.\nKeep it concise.\n"
        );
        assert!(yaml.prune);

        let json = parse_deployment_manifest(
            r#"{"settings":{"slack.status_header_template":"Working — <{session_url}>"}}"#,
        )
        .unwrap();
        assert_eq!(
            json.settings["slack.status_header_template"].stored(),
            "Working — <{session_url}>"
        );
    }

    #[test]
    fn deployment_manifest_rejects_non_scalar_settings() {
        let error = parse_deployment_manifest(
            r#"
settings:
  slack.status_updates:
    nested: false
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("deployment manifest"));
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

    #[test]
    fn agent_workflow_and_permission_commands_are_explorable_from_loom() {
        let root = Cli::command();
        for command in [
            "summary",
            "status",
            "channels",
            "artifacts",
            "issues",
            "permissions",
        ] {
            assert!(
                root.find_subcommand(command).is_some(),
                "top-level help omitted {command}"
            );
        }

        let Cli { cmd, .. } = Cli::try_parse_from([
            "loom",
            "permissions",
            "request",
            "github-repository",
            "acme/widgets",
            "--reason",
            "open the pull request",
        ])
        .unwrap();
        assert!(matches!(
            cmd,
            Cmd::Registered(RegisteredCliCommand::Permissions(PermissionsCmd::Request {
                resource: PermissionRequestResource::GithubRepository { .. }
            }))
        ));

        for (verb, expected) in [
            ("list", "list"),
            ("ls", "list"),
            ("get", "get"),
            ("show", "get"),
        ] {
            let Cli { cmd, .. } = if matches!(verb, "get" | "show") {
                Cli::try_parse_from(["loom", "sessions", verb, "session-1"]).unwrap()
            } else {
                Cli::try_parse_from(["loom", "sessions", verb]).unwrap()
            };
            assert!(
                matches!(
                    (cmd, expected),
                    (
                        Cmd::Registered(RegisteredCliCommand::Sessions(SessionCmd::Ls { .. })),
                        "list"
                    ) | (
                        Cmd::Registered(RegisteredCliCommand::Sessions(SessionCmd::Show { .. })),
                        "get"
                    )
                ),
                "sessions {verb} did not parse as canonical {expected}"
            );
        }
    }
}
