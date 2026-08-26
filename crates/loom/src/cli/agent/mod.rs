//! HTTP-only implementations for Loom's agent-facing commands.
//!
//! An HTTP-only client of loom: every command drives the loom REST API
//! through `weaver-api::Client`, never a local database. The target server is
//! resolved from `$WEAVER_API` (or the address a local loom recorded while
//! serving), authenticated with `$LOOM_TOKEN` when set — the same resolution
//! `loom`'s own CLI uses (see `weaver_api::endpoint`). "Current branch"
//! resolves from `$WEAVER_BRANCH`, set by loom for every session it launches;
//! without it, or without a reachable loom, a command fails with a plain-text
//! error rather than falling back to any local state.
//!
//! One module per command group; this file holds only what more than one of
//! them needs — the client, the branch and channel keys the group commands
//! resolve against, and the small shared formatting helpers.

pub mod artifacts;
pub mod channels;
pub mod config;
pub mod issues;
pub mod session;
pub mod status;
pub mod tags;

pub use artifacts::{run as run_artifact, ArtifactCmd};
pub use channels::{run as run_channel, ChannelCmd};
pub use config::{run as run_settings, ConfigCmd};
pub use issues::{run as run_issue, IssueCmd, IssueTagCmd};
pub use session::{run_chatlog, run_events, run_github_token, run_hook, run_self};
pub use status::{run as run_status, run_summary, StatusCmd};
pub use tags::{run as run_tag, TagCmd};

use anyhow::{anyhow, bail, Result};
use std::sync::OnceLock;

use weaver_api::operations::{branches, Operation, Render};
use weaver_api::{BranchView, Client};

// ---------------------------------------------------------------------------
// The loom client and "current branch" resolution
// ---------------------------------------------------------------------------

/// A client pointed at the server `$WEAVER_API` (or a local loom's recorded
/// address) resolves, authenticated with `$LOOM_TOKEN` when set.
static CLIENT_OVERRIDE: OnceLock<Client> = OnceLock::new();

/// Point compatibility handlers at Loom's fully resolved client context.
/// The standalone binary never calls this and retains environment/local
/// endpoint resolution.
pub fn set_client_override(client: Client) -> Result<()> {
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
/// loom for every session it launches. There is no other way to identify
/// "the current branch" once the CLI no longer reads local git/db state —
/// without it, this only works as a bare client of a server that's told it
/// which branch it's fetching.
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

/// The resolved attention level for a branch: the `attention` tag's value, or
/// `ok` when the branch carries no such tag (absence is calm).
fn attention_of(b: &BranchView) -> String {
    b.tags
        .iter()
        .find(|t| t.key == weaver_core::tags::ATTENTION_KEY)
        .map(|t| t.value.clone())
        .unwrap_or_else(|| "ok".to_string())
}

/// The live status of the branch working an issue, as `"<branch> · <attention>
/// — <message>"`, or `None` when the branch row can't be resolved (a stale
/// `claimed_branch` name, or a network hiccup — best-effort). This is what
/// turns an issue lookup into a poll of a delegated sub-tree.
async fn working_branch_status(client: &Client, repo_root: &str, claimed: &str) -> Option<String> {
    let key = format!("{repo_root}:{claimed}");
    let b = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: key.to_string(),
        })
        .await
        .ok()?;
    let attention = attention_of(&b);
    let status = if b.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", b.description)
    };
    Some(format!("{claimed} · {status}"))
}

/// One operation's own rendering of a result a hand-written command fetched
/// itself. What the declaration prints and what the bespoke command prints are
/// then the same function, so they cannot drift.
fn render<O: Operation + Render>(output: &O::Output) -> String
where
    O::View: Default,
{
    O::text(output, &O::View::default())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_the_max_length() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long string", 6), "a ver…");
    }
}
