//! `loom deployment` — apply a declarative manifest read from a local file.

use crate::client;
use anyhow::{Context, Result};
use clap::Subcommand;
use weaver_api::operations::deployment;

#[derive(Subcommand)]
pub enum DeploymentCmd {
    /// Reconcile settings, profiles, secret references, and workload federation mappings.
    Apply {
        /// YAML (or JSON) manifest path, or `-` for stdin.
        #[arg(long, default_value = "-")]
        file: String,
    },
}

pub async fn run_deployment(cmd: DeploymentCmd) -> Result<()> {
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

pub(crate) fn parse_deployment_manifest(contents: &str) -> Result<deployment::reconcile::Input> {
    serde_yaml_ng::from_str(contents).context("decoding deployment manifest as YAML or JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
