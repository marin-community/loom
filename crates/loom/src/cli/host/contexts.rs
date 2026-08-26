//! `loom login` / `logout` / `context` — this machine's saved credentials.
//!
//! The credential file is host state: these commands read and write it
//! directly, and the prompt for a token never leaves the terminal.

use super::setup::prompt_line;
use crate::client::{self, Client};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use weaver_api::operations::auth;

#[derive(Subcommand)]
pub enum ClientContextCmd {
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

pub async fn cmd_login(name: String, url: Option<String>, token_stdin: bool) -> Result<()> {
    let paths = crate::client_context::ClientPaths::discover()?;
    let existing_url = crate::client_context::context_url(&paths, &name)?;
    let url = match url {
        Some(url) => url,
        None => prompt_line("Server URL", existing_url.as_deref())?,
    };
    let url = crate::client_context::normalize_url(&url)?;
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
    let me = remote.invoke::<auth::me::Op>(&auth::me::Input {}).await?;
    if !me.authenticated || me.via.as_deref() != Some("token") {
        bail!("Loom rejected the personal API token");
    }
    remote
        .invoke::<auth::tokens::list::Op>(&auth::tokens::list::Input {})
        .await
        .context("credential is authenticated but is not a user API token")?;
    crate::client_context::save_login(&paths, &name, &url, token)?;
    let username = me.username.as_deref().unwrap_or("unknown user");
    println!("logged in to {url} as {username}");
    println!("current context: {name}");
    Ok(())
}

pub fn cmd_logout(name: String) -> Result<()> {
    let paths = crate::client_context::ClientPaths::discover()?;
    if crate::client_context::remove_login(&paths, &name)? {
        println!("removed saved credential for {name}");
    } else {
        println!("no saved credential for {name}");
    }
    Ok(())
}

pub fn run_client_context(cmd: ClientContextCmd) -> Result<()> {
    let paths = crate::client_context::ClientPaths::discover()?;
    match cmd {
        ClientContextCmd::Ls => {
            let contexts = crate::client_context::list_contexts(&paths)?;
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
            crate::client_context::use_context(&paths, &name)?;
            println!("current context: {name}");
            Ok(())
        }
        ClientContextCmd::Add {
            name,
            url,
            use_context,
        } => {
            crate::client_context::save_context(&paths, &name, &url, use_context)?;
            println!("saved context {name}");
            Ok(())
        }
        ClientContextCmd::Current => {
            let selection = client::current_selection()?;
            match selection.source {
                client::ClientSelectionSource::Context { name, source } => {
                    let source_name = match source {
                        crate::client_context::ContextSource::Explicit => "--context",
                        crate::client_context::ContextSource::Environment => "LOOM_CONTEXT",
                        crate::client_context::ContextSource::Repository(path) => {
                            println!("selector: {}", path.display());
                            "repository"
                        }
                        crate::client_context::ContextSource::Default => "default",
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
            if crate::client_context::remove_context(&paths, &name)? {
                println!("removed context {name}");
            } else {
                println!("unknown context {name}");
            }
            Ok(())
        }
    }
}
