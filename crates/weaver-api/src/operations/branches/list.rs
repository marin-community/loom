use super::prelude::*;

/// List every branch loom is tracking (fleet-wide, unfiltered).
#[operation(
    id = "branches.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<BranchView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
