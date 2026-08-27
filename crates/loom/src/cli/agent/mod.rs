//! The commands an agent runs from inside its own session.
//!
//! Everything here reaches loom over HTTP through `weaver_api::Client`; none of
//! it opens the database. The server comes from `$WEAVER_API` (or the address a
//! local loom recorded while serving) and the credential from `$LOOM_TOKEN`, the
//! same resolution the rest of the CLI uses — see `weaver_api::endpoint`.
//! "Current branch" comes from `$WEAVER_BRANCH`, which loom sets for every
//! session it launches. Without it, or without a reachable loom, a command fails
//! with a plain-text error.
//!
//! One module per command group. This file holds what more than one of them
//! needs: the client, the branch and channel keys they resolve against, and the
//! shared formatting helpers.
//!
//! Each group's `#[derive(Subcommand)]` enum carries its notes as `//` comments
//! rather than `///`: a doc comment there becomes the group's `about` in
//! `--help`, which is not where an implementation note belongs.

pub mod artifacts;
pub mod channels;
pub mod issues;
pub mod session;
pub mod settings;
pub mod status;
pub mod tags;

pub use artifacts::{run as run_artifact, ArtifactCmd};
pub use channels::{run as run_channel, ChannelCmd};
pub use issues::{run as run_issue, IssueCmd, IssueTagCmd};
pub use session::{run_chatlog, run_events, run_hook, run_self};
pub use settings::{run as run_settings, SettingsCmd};
pub use status::{run as run_status, run_summary, StatusCmd};
pub use tags::{run as run_tag, TagCmd};

use anyhow::{anyhow, bail, Result};
use std::sync::OnceLock;

use weaver_api::operations::{branches, Operation, Render};
use weaver_api::render::sessions::status_line;
use weaver_api::Client;

// The column-fitting helper the group commands share with `loom`'s own.
pub(super) use crate::cli::support::truncate;

// ---------------------------------------------------------------------------
// The loom client and "current branch" resolution
// ---------------------------------------------------------------------------

/// A client pointed at the server `$WEAVER_API` (or a local loom's recorded
/// address) resolves, authenticated with `$LOOM_TOKEN` when set.
static CLIENT_OVERRIDE: OnceLock<Client> = OnceLock::new();

/// Point compatibility handlers at Loom's fully resolved client context.
/// The standalone binary never calls this and retains environment/local
/// endpoint resolution.
pub(crate) fn set_client_override(client: Client) -> Result<()> {
    CLIENT_OVERRIDE
        .set(client)
        .map_err(|_| anyhow!("Loom client override is already set"))
}

pub(crate) fn client() -> Client {
    CLIENT_OVERRIDE
        .get()
        .cloned()
        .unwrap_or_else(weaver_api::endpoint::default_client)
}

/// The branch key every command operates against: `$WEAVER_BRANCH`, set by
/// loom for every session it launches. Required — the CLI is a bare client
/// with no local git/db state to fall back to.
fn branch_key() -> Result<String> {
    let key = std::env::var("WEAVER_BRANCH").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        bail!(
            "not running inside a loom session ($WEAVER_BRANCH is not set) — \
             loom only works inside a session it launched"
        );
    }
    Ok(key.to_string())
}

fn channel_key(explicit: Option<String>) -> Result<String> {
    if let Some(value) = explicit {
        let value = value.trim();
        if !value.is_empty() && value != "self" {
            return Ok(value.to_string());
        }
    }
    let key = std::env::var("LOOM_SESSION_ID").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        bail!("no channel selected and $LOOM_SESSION_ID is not set — pass --channel <id>");
    }
    Ok(key.to_string())
}

/// The live status of the branch working an issue, as `"<branch> · <attention>
/// — <message>"`, or `None` when the branch row can't be resolved (a stale
/// `claimed_branch` name, or a network hiccup — best-effort).
async fn working_branch_status(client: &Client, repo_root: &str, claimed: &str) -> Option<String> {
    let key = format!("{repo_root}:{claimed}");
    let b = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: key.to_string(),
        })
        .await
        .ok()?;
    Some(format!("{claimed} · {}", status_line(&b)))
}

/// An operation's own rendering, for a result a hand-written command fetched
/// itself — so the declaration's output and the bespoke command's output stay
/// the same function.
fn render<O: Operation + Render>(output: &O::Output) -> String
where
    O::View: Default,
{
    O::text(output, &O::View::default())
}
