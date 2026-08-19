use super::prelude::*;

/// Set the branch's attention level and current-state message in one call.
///
/// The branch-scoped twin of `sessions.status.set`, for a target with no live
/// session bound to it (a finished session, or an id naming another branch
/// entirely).
#[operation(
    id = "branches.status.set",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/branches/write@v1"],
    cli = "branches status set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The attention level: `ok`, `attention`, or `blocked`.
    #[operand(long = "tag")]
    pub level: String,
    /// The current-state message shown alongside the level. Absent/empty
    /// leaves the previous message in place.
    pub message: Option<String>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = BranchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
