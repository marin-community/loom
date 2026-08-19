use super::prelude::*;

/// Show one operator-authored custom MCP server's latest definition and
/// validation state.
#[operation(
    id = "mcps.custom.get",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "mcps custom get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Absolute identity, e.g. `/engineering/search/docs`.
    #[operand(positional)]
    pub identity: String,
}

pub type Output = CustomMcpView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
