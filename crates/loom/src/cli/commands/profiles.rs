//! `loom profile` — named session launch profiles and their environment.

use crate::client;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use weaver_api::operations::{profiles, sessions};

#[derive(Subcommand)]
pub enum ProfileCmd {
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
pub struct ProfileAddOpts {
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
    pub mcp: String,
}

#[derive(Subcommand)]
pub enum ProfileEnvCmd {
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

pub async fn run_profile(cmd: ProfileCmd) -> Result<()> {
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
            client
                .invoke::<profiles::delete::Op>(&profiles::delete::Input { name: name.clone() })
                .await?;
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

pub fn parse_mcp_access(value: &str) -> Result<weaver_api::McpAccess> {
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
