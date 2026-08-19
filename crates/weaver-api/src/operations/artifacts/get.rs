use super::prelude::*;

/// Read one artifact or immutable revision.
#[operation(
    id = "artifacts.get",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts get",
    mcp = "loom_artifact::get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// Select an immutable past revision instead of the latest.
    pub rev: Option<i64>,
    /// Read the repository-shared artifact of this name rather than
    /// resolving this branch's own copy first.
    #[operand(default = false)]
    pub repo: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ArtifactView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
