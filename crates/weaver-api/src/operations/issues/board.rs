use super::prelude::*;

/// Every work item across every repository — the dashboard's board.
///
/// This operation uses `scope = Global`, while `issues.list` uses
/// `scope = Repository`. A scope that changes with input cannot be checked
/// by the authorization system, so these are separate operations rather than
/// one with an optional parameter.
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
