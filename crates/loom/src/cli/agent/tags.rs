//! `loom sessions tags` — set, clear, and list a session's free-form tags.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use weaver_api::operations::sessions;
use weaver_api::{BranchView, Client};
use weaver_core::tags;

use super::{branch_key, client};

#[derive(Subcommand)]
pub enum TagCmd {
    /// Set (insert or replace) a tag. The loud keys (`attention`, `triage`)
    /// accept only `attention` or `blocked`; clear them with `tag rm`. Any
    /// other key is free-form. Defaults to the current branch; `--session`
    /// targets another.
    Set {
        /// The tag key, e.g. `attention`, `triage`, or any free-form name.
        key: String,
        /// The value to store.
        value: String,
        /// One-line reason accompanying the tag.
        #[arg(long, default_value = "")]
        note: String,
        /// The session to tag: an id, `repo:branch`, or unambiguous prefix.
        /// Defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
        /// Who is setting it (attribution); defaults to `manual`.
        #[arg(long, default_value = "manual")]
        by: String,
    },
    /// Clear a tag — return that axis to its calm/default (absent) state.
    #[command(name = "delete", visible_alias = "rm")]
    Rm {
        /// The tag key to clear.
        key: String,
        /// The session to clear it on; defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
    },
    /// List every tag on a session (defaults to the current branch).
    #[command(name = "list", visible_alias = "ls")]
    Ls {
        /// The session to list; defaults to the current branch.
        #[arg(long)]
        session: Option<String>,
    },
}

pub async fn run(cmd: TagCmd) -> Result<()> {
    cmd_tag(cmd).await
}

/// Resolve the branch a tag command targets: the named `--session` (an id,
/// `repo:branch`, or unambiguous prefix) when given, else the current branch.
async fn resolve_tag_target(
    client: &Client,
    session: Option<&str>,
) -> Result<(BranchView, String)> {
    let key = match session {
        Some(key) => key.to_string(),
        None => branch_key()?,
    };
    let resolved = client
        .invoke::<sessions::tags::list::Op>(&sessions::tags::list::Input {
            session: key.clone(),
        })
        .await;
    match session {
        Some(spelled) => resolved
            .map(|branch| (branch, key))
            .map_err(|_| anyhow!("no session matching '{spelled}'")),
        None => resolved.map(|branch| (branch, key)),
    }
}

/// Set, clear, or list a tag on a branch. Tags unify the agent's `attention`
/// self-report and a watch's `triage` assessment with any free-form axis.
async fn cmd_tag(cmd: TagCmd) -> Result<()> {
    let client = client();
    match cmd {
        TagCmd::Set {
            key: tag_key,
            value,
            note,
            session,
            by,
        } => {
            let (target, session_key) = resolve_tag_target(&client, session.as_deref()).await?;
            let tag_key = tag_key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if !tags::is_valid_value(tag_key, value) {
                if tags::is_loud(tag_key) {
                    bail!(
                        "'{tag_key}' accepts only {} — use `loom sessions tags delete {tag_key}` to clear it",
                        tags::ATTENTION_VALUES.join(", ")
                    );
                }
                bail!("a tag value cannot be empty — use `loom sessions tags delete {tag_key}` to clear it");
            }
            client
                .invoke::<sessions::tags::set::Op>(&sessions::tags::set::Input {
                    key: tag_key.to_string(),
                    value: value.to_string(),
                    note: note.to_string(),
                    by: Some(by.to_string()),
                    session: session_key,
                })
                .await?;
            if note.is_empty() {
                println!("tag: {} → {tag_key} = {value} (by {by})", target.branch);
            } else {
                println!(
                    "tag: {} → {tag_key} = {value} (by {by}) — {note}",
                    target.branch
                );
            }
        }
        TagCmd::Rm {
            key: tag_key,
            session,
        } => {
            let (target, session_key) = resolve_tag_target(&client, session.as_deref()).await?;
            let tag_key = tag_key.trim();
            client
                .invoke::<sessions::tags::delete::Op>(&sessions::tags::delete::Input {
                    key: tag_key.to_string(),
                    by: Some("manual".to_string()),
                    session: session_key,
                })
                .await?;
            println!("tag: {} → cleared {tag_key}", target.branch);
        }
        TagCmd::Ls { session } => {
            let (target, _) = resolve_tag_target(&client, session.as_deref()).await?;
            if target.tags.is_empty() {
                println!("(no tags)");
                return Ok(());
            }
            for t in &target.tags {
                let by = if t.set_by.is_empty() {
                    String::new()
                } else {
                    format!("  (by {})", t.set_by)
                };
                let note = if t.note.is_empty() {
                    String::new()
                } else {
                    format!("  — {}", t.note)
                };
                println!("{} = {}{by}{note}", t.key, t.value);
            }
        }
    }
    Ok(())
}
