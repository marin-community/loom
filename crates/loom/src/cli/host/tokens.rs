//! `loom token` — the API tokens automation presents as `LOOM_TOKEN`.

use crate::cli::support::truncate;
use crate::client;
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use weaver_api::operations::auth;

#[derive(Subcommand)]
pub enum TokenCmd {
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

pub async fn run_token(cmd: TokenCmd) -> Result<()> {
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

pub async fn cmd_token_create(name: String, expires_days: Option<i64>) -> Result<()> {
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

pub async fn cmd_token_ls() -> Result<()> {
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

pub async fn cmd_token_rm(id: String) -> Result<()> {
    client::default()?
        .invoke::<auth::tokens::revoke::Op>(&auth::tokens::revoke::Input { id: id.clone() })
        .await?;
    println!("revoked token {id}");
    Ok(())
}

/// Parse a `--ttl` duration: a bare number of seconds, or one suffixed with
/// `s`, `m` or `h`.
pub fn parse_ttl(value: &str) -> Result<i64> {
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
