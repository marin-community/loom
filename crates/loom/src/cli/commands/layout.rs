//! The two `loom sessions layout` commands the registry cannot declare.
//!
//! The other twelve are built from `weaver_api::operations::session_layout` and
//! merge in beside these. `collapse` and `expand` are one operation
//! (`session_layout.groups.preference.set`) reached by two spellings that differ
//! only in the `collapsed` they send, and a declaration is one invocation.

use crate::client;
use anyhow::Result;
use clap::Subcommand;
use weaver_api::operations::session_layout;
use weaver_api::operations::{NoView, Render};

#[derive(Subcommand)]
pub enum SessionLayoutCmd {
    /// Collapse one group for the current operator.
    Collapse { group: String },
    /// Expand one group for the current operator.
    Expand { group: String },
}

/// Dispatch the two `loom sessions layout` commands that stay hand-written.
///
/// Both print through the operation's own renderer, so the twelve declared
/// layout commands and these two describe a layout the same way.
pub(crate) async fn run_session_layout(cmd: SessionLayoutCmd) -> Result<()> {
    let client = client::default()?;
    let (group, collapsed) = match cmd {
        SessionLayoutCmd::Collapse { group } => (group, true),
        SessionLayoutCmd::Expand { group } => (group, false),
    };
    let layout = client
        .invoke::<session_layout::groups::preference::set::Op>(
            &session_layout::groups::preference::set::Input {
                id: group,
                collapsed,
            },
        )
        .await?;
    println!(
        "{}",
        <session_layout::groups::preference::set::Op as Render>::text(&layout, &NoView)
    );
    Ok(())
}
