use super::prelude::*;

/// Remove one free-form tag from a branch — the branch-scoped twin of
/// `sessions.tags.delete`.
#[operation(
    id = "branches.tags.delete",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/branches/write@v1"],
    cli = "branches tags delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The tag key to remove.
    #[operand(positional)]
    pub key: String,
    /// Who is clearing it (a watch name, or blank for `manual`).
    pub by: Option<String>,
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
