//! `loom mcp` — custom MCP servers and the capability registry.
//!
//! `show` searches one response for a custom server *or* a capability set, so
//! the answer depends on which collection holds the name. `add` reads a script
//! and its tests off disk, then chooses create or update from what the registry
//! already holds. `serve` and `serve-custom` are long-running stdio servers.
//! The registry table itself is `loom mcps get`, rendered by
//! `weaver_api::render::mcps`.

use crate::client;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use weaver_api::operations::mcps;

#[derive(Subcommand)]
pub enum McpCmd {
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
pub struct McpAddOpts {
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

pub async fn run_mcp(cmd: McpCmd) -> Result<()> {
    match cmd {
        McpCmd::Serve { adapter } => crate::mcp::serve(&adapter).await,
        McpCmd::ServeCustom { identity } => crate::custom_mcp::serve_from_env(&identity).await,
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
            client::default()?
                .invoke::<mcps::custom::delete::Op>(&mcps::custom::delete::Input {
                    identity: identity.clone(),
                })
                .await?;
            println!("removed {identity}");
            Ok(())
        }
    }
}
