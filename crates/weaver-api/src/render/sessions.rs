//! Text rendering for session operations.
//!
//! The three projections of `BranchView` below are `pub` because they are the
//! session's status vocabulary, not private formatting: `loom summary` and the
//! delegated-sub-tree line in `loom issues` report the same level and message
//! this renderer does, and a second copy of "absence means `ok`" is exactly the
//! drift this module exists to prevent.

use weaver_core::tags;

use crate::dto::BranchView;
use crate::operations::sessions;
use crate::operations::{NoView, Render};

/// The resolved attention level: the `attention` tag's value, or `ok` when the
/// session carries no such tag — absence is the calm state, not a stored `ok`.
pub fn attention(branch: &BranchView) -> String {
    branch
        .tags
        .iter()
        .find(|tag| tag.key == tags::ATTENTION_KEY)
        .map(|tag| tag.value.clone())
        .unwrap_or_else(|| "ok".to_string())
}

/// The level and the current-state message as one phrase. The message persists
/// across a bare level change, so the two are reported together or the level
/// reads as though it had wiped what the agent last said.
pub fn status_line(branch: &BranchView) -> String {
    let attention = attention(branch);
    if branch.description.is_empty() {
        attention
    } else {
        format!("{attention} — {}", branch.description)
    }
}

/// The GitHub thread this session mirrors its status trail onto, as
/// `owner/name#number`, or `None` when it mirrors nowhere.
pub fn github_wiring(branch: &BranchView) -> Option<&str> {
    branch
        .tags
        .iter()
        .find(|tag| tag.key == tags::GITHUB_KEY)
        .map(|tag| tag.value.as_str())
        .filter(|value| !value.is_empty())
}

impl Render for sessions::status::get::Op {
    fn text(output: &BranchView, _: &NoView) -> String {
        let mut lines = vec![
            format!("repo:        {}", output.repo_root),
            format!("branch:      {}", output.branch),
            format!("base:        {}", output.base_branch),
        ];
        if !output.title.is_empty() {
            lines.push(format!("title:       {}", output.title));
        }
        lines.push(format!(
            "goal:        {}",
            if output.goal.is_empty() {
                "(none)"
            } else {
                &output.goal
            }
        ));
        lines.push(format!("status:      {}", status_line(output)));
        if let Some(wiring) = github_wiring(output) {
            lines.push(format!(
                "github:      status messages mirror publicly to {wiring}"
            ));
        }
        lines.push(format!("open issues: {}", output.open_issue_count));
        lines.join("\n")
    }
}

impl Render for sessions::status::set::Op {
    // The session's status as it now stands, not the arguments that were sent:
    // `--tag blocked` with no message keeps the previous message, and reporting
    // the response is how the caller sees that.
    fn text(output: &BranchView, _: &NoView) -> String {
        format!("status: {}", status_line(output))
    }
}
