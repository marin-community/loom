//! The GitHub pull-request snapshot model.
//!
//! [`GithubStatus`] is the per-branch PR snapshot stored in the `branch_github`
//! table and served under `BranchView::github` — a plain serializable row. The
//! polling that *produces* it (shelling out to the `gh` CLI) lives in the loom
//! orchestrator; this is only the model and its serialization, shared by the
//! daemon and the API surface.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;

/// A branch's pull-request snapshot, as stored and as served under
/// `BranchView::github`. `pr_state` is `OPEN` / `CLOSED` / `MERGED`; `checks` is
/// the rolled-up `passing` / `failing` / `pending` (or `null` when the PR has no
/// checks); `review_decision` is GitHub's `APPROVED` / `CHANGES_REQUESTED` /
/// `REVIEW_REQUIRED` (or `null` when review isn't required). `head_updated_at`
/// is the time associated with the current `head_sha`; the poller preserves it
/// while the head is unchanged so unrelated PR metadata does not make code look
/// newly pushed.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GithubStatus {
    pub pr_number: i64,
    pub pr_url: String,
    pub pr_state: String,
    pub pr_title: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub checks: Option<String>,
    pub mergeable: Option<String>,
    pub merged_at: Option<String>,
    pub head_sha: Option<String>,
    pub head_updated_at: Option<String>,
    pub fetched_at: String,
}

impl GithubStatus {
    /// The fields whose change is worth announcing on the activity feed — PR
    /// identity and the human-meaningful state. `mergeable` and timestamps are
    /// deliberately excluded: they flap (e.g. `UNKNOWN` ⇄ `MERGEABLE`) without
    /// telling the user anything.
    pub fn meaningfully_differs_from(&self, other: &Self) -> bool {
        self.pr_number != other.pr_number
            || self.pr_state != other.pr_state
            || self.review_decision != other.review_decision
            || self.checks != other.checks
            || self.is_draft != other.is_draft
            || self.head_sha != other.head_sha
    }

    /// Payload for the `github` event the poller records when the snapshot
    /// changes — enough for a dashboard to summarize without re-fetching.
    pub fn event_data(&self) -> Value {
        json!({
            "pr": self.pr_number,
            "url": self.pr_url,
            "state": self.pr_state,
            "review": self.review_decision,
            "checks": self.checks,
            "draft": self.is_draft,
            "head_sha": self.head_sha,
            "head_updated_at": self.head_updated_at,
        })
    }
}
