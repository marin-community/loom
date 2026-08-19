use super::prelude::*;

/// Resolve an artifact review thread.
#[operation(
    id = "artifacts.threads.resolve",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts resolve",
    mcp = "loom_artifact::resolve",
)]
pub struct Resolve;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// The thread to resolve.
    #[operand(positional)]
    pub thread_id: i64,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ThreadDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
