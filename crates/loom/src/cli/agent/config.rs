//! `loom settings get <key>` — one row out of the settings table.
//!
//! The whole table is `loom settings list`, which is `settings.get` rendered by
//! its own declaration. Picking one key stays here: the operation takes no key,
//! and an unknown one has to fail — a renderer returns text, never an error.

use anyhow::{bail, Result};
use clap::Subcommand;

use weaver_api::operations::settings;

use super::client;

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Print one setting's value.
    Get { key: String },
}

pub async fn run(cmd: ConfigCmd) -> Result<()> {
    cmd_config(cmd).await
}

async fn cmd_config(cmd: ConfigCmd) -> Result<()> {
    let client = client();
    match cmd {
        ConfigCmd::Get { key } => {
            let settings = client
                .invoke::<settings::get::Op>(&settings::get::Input {})
                .await?;
            match settings.settings.iter().find(|s| s.key == key) {
                Some(s) => println!("{}", s.value),
                None => bail!("no setting '{key}' — see `loom settings list`"),
            }
        }
    }
    Ok(())
}
