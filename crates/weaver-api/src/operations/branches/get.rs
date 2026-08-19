use super::prelude::*;

/// Inspect one branch.
#[operation(
    id = "branches.get",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
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
