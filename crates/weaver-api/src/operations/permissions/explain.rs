use super::prelude::*;

/// Explain one registered operation's actor, risk, and projections.
#[operation(
    id = "permissions.explain",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions explain",
    mcp = "loom_permission::explain",
)]
pub struct Explain;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The operation id to explain, e.g. `issues.tags.set`.
    #[operand(positional)]
    pub operation: String,
}

pub type Output = crate::operations::OperationView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
