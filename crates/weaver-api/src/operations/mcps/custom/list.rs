use super::prelude::*;

/// List operator-authored custom MCP servers.
#[operation(
    id = "mcps.custom.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "mcps custom list",
    cli_alias = "ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<CustomMcpView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
