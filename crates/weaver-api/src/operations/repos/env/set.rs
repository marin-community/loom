use super::prelude::*;

/// Upsert one per-repo environment variable. The name is validated as a shell
/// identifier that isn't one of loom's reserved control or GitHub credential
/// names, so it can't corrupt or shadow the launch environment. Returns the
/// refreshed metadata list (no values).
#[operation(
    id = "repos.env.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos env set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The variable's name.
    #[operand(positional)]
    pub name: String,
    /// The value to store.
    #[operand(positional)]
    pub value: String,
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
