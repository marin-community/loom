use super::prelude::*;

/// Update a branch's title, goal, or current-state description.
///
/// A rename requires `expected_title`/`expected_title_provenance` — the label
/// the caller last observed — so a concurrent rename is rejected with a 409
/// instead of silently overwritten.
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
    /// Required with `title`: the task label the caller observed.
    pub expected_title: Option<String>,
    /// Required with `title`: the provenance the caller observed alongside
    /// `expected_title`.
    pub expected_title_provenance: Option<String>,
    pub goal: Option<String>,
    /// The agent's current-state message — the prose shown beside the
    /// attention level. Prefer `branches.status.set` to update this alongside
    /// the level in one call.
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
