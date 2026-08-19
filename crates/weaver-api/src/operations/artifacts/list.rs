use super::prelude::*;

/// List branch and repository-scoped artifacts.
#[operation(
    id = "artifacts.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts list",
    mcp = "loom_artifact::list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// When true, list every artifact in the repository. By default, list
    /// only this branch's own artifacts and the repository-shared ones.
    #[operand(default = false)]
    pub repo: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ArtifactMeta>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
