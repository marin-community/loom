use super::prelude::*;

/// The trusted MCP registry: built-in adapters, versioned capability sets,
/// and operator-authored custom servers.
#[operation(
    id = "mcps.get",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "mcps get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = McpRegistryView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
