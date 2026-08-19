use super::prelude::*;

/// List immutable artifact revisions.
#[operation(
    id = "artifacts.history",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts history",
    mcp = "loom_artifact::history",
)]
pub struct History;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// When true, list the repository-shared artifact's history. By default,
    /// list this branch's own copy.
    #[operand(default = false)]
    pub repo: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ArtifactVersion>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
