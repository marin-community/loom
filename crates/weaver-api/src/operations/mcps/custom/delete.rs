use super::prelude::*;

/// Permanently remove an operator-authored custom MCP server.
#[operation(
    id = "mcps.custom.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "mcps custom delete",
    cli_alias = "rm",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Absolute identity, e.g. `/engineering/search/docs`.
    #[operand(positional)]
    pub identity: String,
}

pub type Output = CustomMcpDeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
