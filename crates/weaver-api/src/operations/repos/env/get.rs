use super::prelude::*;

/// Read a repo's environment variables' metadata: names and timestamps only
/// — values are write-only and never returned.
#[operation(
    id = "repos.env.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos env get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
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
