use super::prelude::*;

/// Set one free-form tag on a branch — the branch-scoped twin of
/// `sessions.tags.set`, for a target with no live session bound to it (a
/// finished session, or an id naming another branch entirely).
#[operation(
    id = "branches.tags.set",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/branches/write@v1"],
    cli = "branches tags set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The tag key.
    #[operand(positional)]
    pub key: String,
    /// The tag value.
    #[operand(positional)]
    pub value: String,
    /// One-line reason accompanying the tag.
    #[operand(default = String::new())]
    pub note: String,
    /// Who is setting it (a watch name, or blank for `manual`).
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
