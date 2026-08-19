use super::prelude::*;

/// Update a branch's title, goal, or current-state description.
///
/// Title updates require `expected_title` and `expected_title_provenance`
/// to detect and reject concurrent renames.
#[operation(
    id = "branches.update",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/branches/write@v1"],
    cli = "branches update",
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    pub title: Option<String>,
    /// Required with `title`.
    pub expected_title: Option<String>,
    /// Required with `title`.
    pub expected_title_provenance: Option<String>,
    pub goal: Option<String>,
    /// The agent's current-state message.
    pub description: Option<String>,
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
