//! loom — the orchestration CLI.
//!
//! Most subcommands talk to the running loom daemon over HTTP (session
//! lifecycle, archive, adopt). `loom server run` runs the daemon in the
//! foreground; `loom server start`/`stop`/`restart`/`status` manage its
//! background lifecycle. To interact with an agent, use `loom sessions attach`
//! or the browser terminal.

use anyhow::{bail, Context, Result};
use clap::{ArgMatches, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use loom::cli::agent::{
    ArtifactCmd as AgentArtifactCmd, ChannelCmd as AgentChannelCmd, IssueCmd as AgentIssueCmd,
    SettingsCmd, StatusCmd as AgentStatusCmd,
};

use loom::cli::commands::mcps::{run_mcp, McpCmd};
use loom::cli::commands::permissions::{run_permissions, PermissionsCmd};
use loom::cli::commands::profiles::{run_profile, ProfileCmd};
use loom::cli::commands::review::{run_review, ReviewCmd};
use loom::cli::commands::sessions::{
    cmd_attach, cmd_launch, cmd_ps, run_session, AttachArgs, LaunchOpts, PsOptions, SessionCmd,
};

use loom::cli::commands::watches::{run_watch, WatchCmd};
use loom::cli::host::config::{run_config, ConfigCmd};
use loom::cli::host::contexts::{cmd_login, cmd_logout, run_client_context, ClientContextCmd};
use loom::cli::host::deployment::{run_deployment, DeploymentCmd};
use loom::cli::host::federation::{run_federation, FederationCmd};
use loom::cli::host::server::{cmd_open, run_server, ServerCmd};
use loom::cli::host::setup::{run_setup, SetupCmd};
use loom::cli::host::tokens::{run_token, TokenCmd};
use loom::cli::support::configure_agent_client;
use loom::client;

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
        cmd: SettingsCmd,
    },
    /// Inspect trusted MCP capability sets, or run an internal stdio adapter.
    #[command(name = "mcps", visible_alias = "mcp")]
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
    #[command(name = "profiles", visible_alias = "profile")]
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
    /// `loom.toml` is the only place every credential/setting is written —
    /// the shared contract deployment tooling consumes:
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
}

#[allow(clippy::large_enum_variant)]
enum Cmd {
    Registered(RegisteredCliCommand),
    /// An operation reached through the generic registry dispatcher.
    ///
    /// No per-command code: the clap command, the request, and the printing all
    /// come from the operation's own declaration.
    Operation(loom::cli::CliBinding, ArgMatches),
    Host(HostCmd),
}

/// Registry bindings whose command name no hand-written command already claims.
///
/// Both kinds coexist deliberately. A hand-written command exists because its
/// output is worth formatting by hand; the generic dispatcher exists so that
/// declaring an operation makes it reachable immediately, with no second edit.
fn generic_bindings() -> Vec<loom::cli::CliBinding> {
    loom::cli::bindings()
        .into_iter()
        .filter(|binding| {
            // Skip only when the hand-written tree already offers this exact
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

/// `loom hook --event <name>` — the Claude Code lifecycle hook entry point.
///
/// Hand-written because it is not an operation: it stamps a branch event from
/// the environment a hook runs in, and does nothing at all when that
/// environment says the caller is a nested agent.
fn hook_command() -> Command {
    HookArgs::augment_args(
        Command::new("hook").about("Record a Claude Code lifecycle hook for this session"),
    )
}

fn parse_hook_command(matches: &ArgMatches) -> clap::error::Result<RegisteredCliCommand> {
    HookArgs::from_arg_matches(matches).map(|args| RegisteredCliCommand::Hook(args.event))
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

const PERMISSION_CLI_COMMANDS: &[CliCommandFactory] = &[CliCommandFactory {
    name: "permissions",
    aliases: &[],
    build: permissions_command,
    parse: parse_permissions_command,
}];

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
        // Generic bindings go first: whatever resolves here is an operation no
        // bespoke command implements. Asking the hand-written factory table
        // first would match on the *group* name, letting a group with any
        // hand-written command swallow every declared operation beside it
        // (e.g. `loom issues list`), even though the tree, `--help`, and
        // `/api/operations` all advertise it as reachable.
        let bindings = generic_bindings();
        if let Some((binding, operation_matches)) = loom::cli::resolve(&bindings, matches) {
            return Ok(Self::Operation(*binding, operation_matches));
        }
        if let Some((name, command_matches)) = matches.subcommand() {
            if let Some(factory) = registered_cli_factory(name) {
                return (factory.parse)(command_matches).map(Self::Registered);
            }
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
    ///
    /// Checking bindings against the registry is not enough — a descriptor and
    /// a clap enum can each look right while naming different commands, so
    /// only walking the built tree catches the drift.
    ///
    /// It covers hand-written commands too, deliberately: an operation served
    /// by a bespoke command still has to be reachable by the name it
    /// advertises.
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
            // Landing on a *group* means the advertised words reach something
            // else entirely, e.g. `loom context` — the credential-context
            // manager, which takes a subcommand of its own, not a runnable
            // leaf. An operation is one invocation, so what it names has to
            // be runnable.
            if let Some(leaf) = node {
                if leaf.get_subcommands().next().is_some() {
                    drift.push(format!(
                        "  {} advertises `{}`, which is a command group, not an invocation",
                        operation.id,
                        cli.invocation()
                    ));
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

    /// Every generic binding is *dispatched to*, not merely present.
    ///
    /// Walking the built tree and running the binary are different questions:
    /// a command can sit in the tree, documented by `--help` and advertised by
    /// `/api/operations`, yet never be reached if `Cmd::from_arg_matches`
    /// resolves its group name to a hand-written command first.
    #[test]
    fn every_generic_binding_dispatches_to_its_operation() {
        let command = Cli::command();
        let mut drift = Vec::new();
        for binding in generic_bindings() {
            let Some(cli) = binding.operation.cli else {
                continue;
            };
            let argv = smallest_invocation(&command, cli.path);
            let spelled = argv[1..].join(" ");
            let matches = match command.clone().try_get_matches_from(&argv) {
                Ok(matches) => matches,
                Err(error) => {
                    drift.push(format!("  `loom {spelled}` does not parse: {error}"));
                    continue;
                }
            };
            match Cmd::from_arg_matches(&matches) {
                Ok(Cmd::Operation(resolved, _))
                    if resolved.operation.id == binding.operation.id => {}
                Ok(Cmd::Operation(resolved, _)) => drift.push(format!(
                    "  `loom {spelled}` is {} but dispatched to {}",
                    binding.operation.id, resolved.operation.id
                )),
                Ok(_) => drift.push(format!(
                    "  `loom {spelled}` is {} but dispatched to a hand-written command",
                    binding.operation.id
                )),
                Err(error) => drift.push(format!(
                    "  `loom {spelled}` is {} but dispatch refused it: {error}",
                    binding.operation.id
                )),
            }
        }
        assert!(
            drift.is_empty(),
            "{} advertised command(s) do not reach the operation they name:\n{}",
            drift.len(),
            drift.join("\n")
        );
    }

    /// The shortest argv that gets `path` past clap: the words themselves, then
    /// a value for each argument the leaf insists on.
    ///
    /// `1` satisfies every value parser the tree uses — the string one and the
    /// integer one — so the arguments need no introspection beyond whether they
    /// are required and whether they are positional.
    fn smallest_invocation(command: &Command, path: &[&str]) -> Vec<String> {
        let mut node = command;
        for segment in path {
            node = node
                .get_subcommands()
                .find(|candidate| {
                    candidate.get_name() == *segment
                        || candidate.get_all_aliases().any(|alias| alias == *segment)
                })
                .unwrap_or_else(|| panic!("`{segment}` is not in the built tree"));
        }
        let mut argv: Vec<String> = std::iter::once("loom".to_string())
            .chain(path.iter().map(|segment| (*segment).to_string()))
            .collect();
        let required: Vec<&clap::Arg> = node
            .get_arguments()
            .filter(|argument| argument.is_required_set())
            .collect();
        for argument in required.iter().filter(|argument| !argument.is_positional()) {
            let long = argument
                .get_long()
                .expect("a required option is spelled with a long flag");
            argv.push(format!("--{long}"));
            argv.push("1".to_string());
        }
        for _ in required.iter().filter(|argument| argument.is_positional()) {
            argv.push("1".to_string());
        }
        argv
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
            // Printed as the renderer built it: trimming would truncate an
            // artifact's own bytes (`loom artifacts get plan > plan.md`); the
            // only addition is the final newline a terminal expects.
            if !rendered.is_empty() {
                if rendered.ends_with('\n') {
                    print!("{rendered}");
                } else {
                    println!("{rendered}");
                }
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
            loom::cli::agent::run_summary().await
        }
        RegisteredCliCommand::SelfContext => {
            configure_agent_client()?;
            loom::cli::agent::run_self().await
        }
        RegisteredCliCommand::Status(cmd) => {
            configure_agent_client()?;
            loom::cli::agent::run_status(cmd).await
        }
        RegisteredCliCommand::Sessions(cmd) => run_session(cmd).await,
        RegisteredCliCommand::Launch(opts) => cmd_launch(opts.into()).await,
        RegisteredCliCommand::Ps => cmd_ps(PsOptions::default()).await,
        RegisteredCliCommand::Attach(session) => cmd_attach(session).await,
        RegisteredCliCommand::Hook(event) => {
            configure_agent_client()?;
            loom::cli::agent::run_hook(event).await
        }
        RegisteredCliCommand::Channels(cmd) => {
            configure_agent_client()?;
            loom::cli::agent::run_channel(cmd).await
        }
        RegisteredCliCommand::Artifacts(cmd) => {
            configure_agent_client()?;
            loom::cli::agent::run_artifact(cmd).await
        }
        RegisteredCliCommand::Review(cmd) => run_review(cmd).await,
        RegisteredCliCommand::Issues(cmd) => {
            configure_agent_client()?;
            loom::cli::agent::run_issue(cmd).await
        }
        RegisteredCliCommand::Permissions(cmd) => run_permissions(cmd).await,
    }
}

async fn run_host_cli(command: HostCmd) -> Result<()> {
    match command {
        HostCmd::Help { topic, json } => run_help(topic, json),
        HostCmd::Doctor => run_doctor().await,
        HostCmd::Settings { cmd } => {
            configure_agent_client()?;
            loom::cli::agent::run_settings(cmd).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use loom::cli::commands::permissions::PermissionRequestResource;
    use loom::cli::commands::profiles::parse_mcp_access;

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

    /// Every `loom review` word still parses, and each reaches the half that
    /// owns it.
    ///
    /// The bundle is split: five commands come from their operation's
    /// declaration and are dispatched generically, and the rest stay
    /// hand-written because one declaration cannot express what they do. A
    /// command silently changing sides is exactly the drift this catches.
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
            vec!["loom", "review", "resolve", "4", "9"],
            vec!["loom", "review", "reopen", "4", "9"],
            vec![
                "loom",
                "review",
                "submit",
                "4",
                "--revision",
                "2",
                "--acknowledge-outdated",
            ],
        ] {
            let spelled = args.join(" ");
            assert!(
                matches!(
                    Cli::try_parse_from(args).unwrap().cmd,
                    Cmd::Registered(RegisteredCliCommand::Review(_))
                ),
                "`{spelled}` no longer reaches its hand-written command"
            );
        }

        for (args, id) in [
            (vec!["loom", "review", "show", "4"], "reviews.get"),
            (
                vec![
                    "loom",
                    "review",
                    "delete-comment",
                    "4",
                    "9",
                    "--revision",
                    "2",
                ],
                "reviews.comments.delete",
            ),
            (
                vec!["loom", "review", "discard", "4", "--revision", "2"],
                "reviews.discard",
            ),
            (
                vec!["loom", "review", "retarget", "4", "--revision", "2"],
                "reviews.retarget",
            ),
            (
                vec!["loom", "review", "retry", "4"],
                "reviews.retry_delivery",
            ),
        ] {
            let spelled = args.join(" ");
            let Cmd::Operation(binding, _) = Cli::try_parse_from(args).unwrap().cmd else {
                panic!("`{spelled}` is declared by {id} but did not dispatch generically");
            };
            assert_eq!(
                binding.operation.id, id,
                "`{spelled}` reached the wrong operation"
            );
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
