//! `loom federation` — the trusted GitHub Actions OIDC workflow mappings.

use crate::client;
use anyhow::Result;
use clap::{Args, Subcommand};
use weaver_api::operations::auth;

#[derive(Args)]
pub struct FederationAddArgs {
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
pub enum FederationCmd {
    Add(Box<FederationAddArgs>),
    Ls,
    Rm { id: String },
}

pub async fn run_federation(cmd: FederationCmd) -> Result<()> {
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
            client
                .invoke::<auth::federations::remove::Op>(&auth::federations::remove::Input {
                    id: id.clone(),
                })
                .await?;
            println!("removed federation mapping {id}");
        }
    }
    Ok(())
}
