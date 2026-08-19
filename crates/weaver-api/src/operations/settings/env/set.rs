use super::prelude::*;

/// Upsert one variable in the default profile's environment. The value is
/// free-form; the name is validated as a shell identifier so it cannot
/// corrupt the launch script that exports it.
#[operation(
    id = "settings.env.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings env set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The variable name — a POSIX-portable shell identifier.
    #[operand(positional)]
    pub name: String,
    /// The value to store.
    #[operand(positional)]
    pub value: String,
}

pub type Output = Vec<AgentEnvVarView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
