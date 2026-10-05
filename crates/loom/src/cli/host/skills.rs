//! `loom skills install` — copy an operator-supplied skill into every
//! installed harness's global skills directory.
//!
//! Host-local by design: this is machine provisioning (the entrypoint calls it
//! after `LOOM_INSTALL_CMD` on every boot), not a fleet capability, so it
//! never touches the API or the database. Loom owns *placement* only — the
//! payload comes from a path the operator names, typically one staged by
//! `LOOM_INSTALL_CMD`.

use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::PathBuf;

use loom_agent::skills::{install_skill, SkillInstallStatus, SUPPORTED_HARNESSES};

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// Install a skill into every installed harness's global skills dir.
    ///
    /// A file source lands as `<skills>/<name>/SKILL.md`; a directory source
    /// is copied recursively, keeping its inner layout. Harnesses whose
    /// binary is not on PATH are skipped; per-harness copy failures warn and
    /// continue.
    Install {
        /// The skill's name — the directory each harness will see it under.
        name: String,
        /// The skill source: a SKILL.md file, or a skill directory.
        source: PathBuf,
    },
}

pub async fn run_skills(cmd: SkillsCmd) -> Result<()> {
    let SkillsCmd::Install { name, source } = cmd;
    let report = install_skill(&name, &source).await?;
    if report.harnesses.is_empty() {
        bail!(
            "no agent harness is installed on this machine (expected one of {} on PATH)",
            SUPPORTED_HARNESSES.join(", ")
        );
    }
    for harness in &report.harnesses {
        match harness.status {
            SkillInstallStatus::Installed => println!(
                "installed '{name}' for {} ({})",
                harness.harness,
                harness.skills_dir.join(&name).display()
            ),
            SkillInstallStatus::UpToDate => {
                println!("'{name}' already up to date for {}", harness.harness)
            }
            SkillInstallStatus::Failed => eprintln!(
                "warning: '{name}' not installed for {}: {}",
                harness.harness,
                harness.error.as_deref().unwrap_or("copy failed")
            ),
        }
    }
    if !report.any_installed() {
        bail!("skill '{name}' was not installed for any harness");
    }
    Ok(())
}
