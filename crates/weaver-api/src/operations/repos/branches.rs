use super::prelude::*;

/// List the local git branches of a repo checkout, and which has a worktree.
///
/// `cwd` is a server-local filesystem path (any git checkout the server
/// process can read), not a managed-repo slug.
#[operation(
    id = "repos.branches",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos branches",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A path inside the repo checkout to list branches for.
    #[operand(positional)]
    pub cwd: String,
}

pub type Output = Vec<RepoBranchView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
