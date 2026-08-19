use super::prelude::*;

/// Edit a work item's own fields.
///
/// Claiming is not here: a claim is made by launching a session against an
/// item, so the only claim change this expresses is `unclaim`, which returns the
/// item to the backlog. The route this replaces spelled that as
/// `claimed_branch: null` and rejected every other value with a 400 — an
/// `Option<Option<String>>` whose only legal inhabitant was `Some(None)`.
#[operation(
    id = "issues.update",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues update",
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A Loom work-item id.
    #[operand(positional)]
    pub id: i64,
    /// Replace the one-line summary.
    pub title: Option<String>,
    /// Replace the detail body.
    pub body: Option<String>,
    /// `open` or `closed`.
    pub status: Option<String>,
    /// GitHub issue mapping as `owner/name#number`. An empty string clears the
    /// mapping; omitting the field leaves it unchanged.
    pub github: Option<String>,
    /// Return the item to the unclaimed backlog.
    #[operand(default = false)]
    pub unclaim: bool,
    #[operand(context)]
    pub repo_root: String,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
