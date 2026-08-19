use super::prelude::*;

/// List available agent runtimes: builtins, operator-defined custom agents,
/// and the configured default.
#[operation(
    id = "agents.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/agents/read@v1"],
    cli = "agents list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = AgentsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
