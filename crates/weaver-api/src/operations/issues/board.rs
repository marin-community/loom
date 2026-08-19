use super::prelude::*;

/// Every work item across every repository — the dashboard's board.
///
/// A different read from [`super::list`], not a wider one: `issues.list` is
/// `scope = Repository` and answers "what is happening in this repo", which is
/// the question an agent asks. This one is `scope = Global` and answers "what is
/// happening anywhere", which is the question the board asks, and it is why the
/// two are separate operations rather than one with an optional `repo_root` —
/// a scope that changes with the input is a scope nothing can check.
#[operation(
    id = "issues.board",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/issues/read@v1"],
    cli = "issues board",
)]
pub struct Board;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Include closed work items.
    #[operand(default = false)]
    pub all: bool,
    /// Include items claimed by an automation-class session's branch. Defaults
    /// to `false` — the board shows the work of the interactive fleet, not the
    /// trackers its machinery opens for itself.
    #[operand(default = false)]
    pub automation: bool,
}

pub type Output = Vec<IssueView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
