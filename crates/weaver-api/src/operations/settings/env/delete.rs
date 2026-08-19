use super::prelude::*;

/// Remove one variable from the default profile's environment. A missing
/// name is not an error — the desired end state already holds.
#[operation(
    id = "settings.env.delete",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings env delete",
    cli_alias = "rm",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The variable name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = Vec<AgentEnvVarView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
