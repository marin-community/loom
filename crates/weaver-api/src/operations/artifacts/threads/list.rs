use super::prelude::*;

/// List anchored artifact review threads.
#[operation(
    id = "artifacts.threads.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts threads",
    mcp = "loom_artifact::threads",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// When true, list only unresolved threads. By default, include all threads.
    /// Resolved threads appear collapsed in the dashboard, not hidden.
    #[operand(default = false)]
    pub open_only: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ThreadDto>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
