use super::prelude::*;

/// Remove a custom agent. Removing an absent name is a no-op. Sessions
/// already launched with it are unaffected.
///
/// Operator-only, same reasoning as `agents.custom.create`.
#[operation(
    id = "agents.custom.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "agents custom delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The custom agent's name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = CustomAgentsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
