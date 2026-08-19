use super::prelude::*;

/// Delete an artifact and its complete revision history.
#[operation(
    id = "artifacts.delete",
    actor = SessionSelf,
    scope = Branch,
    risk = Destructive,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts delete",
    mcp = "loom_artifact::delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// Delete the repository-shared artifact instead of this branch's own
    /// copy.
    #[operand(default = false)]
    pub repo: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ArtifactDeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
