use super::prelude::*;

/// Check whether a worktree fork point resolves against a repo checkout,
/// matching what a launch would fork from — fetching the revision from
/// `origin` on demand if needed. Never touches local branches or the working
/// tree.
#[operation(
    id = "repos.revisions.validate",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos revisions validate",
)]
pub struct Validate;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A path inside the repo checkout to validate against.
    #[operand(positional)]
    pub cwd: String,
    /// The revision (branch, tag, or ref) to resolve.
    #[operand(positional)]
    pub revision: String,
}

pub type Output = RepoRevisionValidationView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
