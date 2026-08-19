use super::prelude::*;

/// List work items claimed by this branch — the session's working set.
///
/// Distinct from `issues.list` (repository-scoped, keyed by `repo_root`):
/// this is the branch-scoped view `GET /branches/{id}/issues` served, which
/// no operation in the `issues` bundle currently covers.
#[operation(
    id = "branches.issues.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches issues list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Include closed work items.
    #[operand(default = false)]
    pub all: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<IssueView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
