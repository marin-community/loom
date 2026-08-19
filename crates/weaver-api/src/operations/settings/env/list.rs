use super::prelude::*;

/// List every variable in the default profile's environment. Unlike a named
/// profile's environment metadata, values are returned in full.
#[operation(
    id = "settings.env.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "settings env list",
    cli_alias = "ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<AgentEnvVarView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
