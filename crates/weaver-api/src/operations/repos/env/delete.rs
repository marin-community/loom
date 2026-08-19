use super::prelude::*;

/// Remove one per-repo environment variable. Removing an absent name is a
/// no-op. Returns the refreshed metadata list (no values).
#[operation(
    id = "repos.env.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos env delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The variable's name.
    #[operand(positional)]
    pub name: String,
    /// Repo to scope to (canonical primary-worktree path). One of
    /// `repo_root`/`cwd` is required.
    pub repo_root: Option<String>,
    /// A directory inside the repo, resolved server-side when `repo_root` is
    /// omitted.
    pub cwd: Option<String>,
}

pub type Output = RepoEnvView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
