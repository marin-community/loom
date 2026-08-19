use super::prelude::*;

/// List recent durable events on a branch (newest first, last 200 entries).
#[operation(
    id = "branches.events.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches events list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<weaver_core::events::Event>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
