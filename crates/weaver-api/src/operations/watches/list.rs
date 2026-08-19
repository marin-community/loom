use super::prelude::*;

/// List every registered watch: name, enabled, trigger, program, last outcome.
#[operation(
    id = "watches.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<WatchView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
