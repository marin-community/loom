use super::prelude::*;

/// Permanently delete a named launch profile.
#[operation(
    id = "profiles.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "profiles delete",
    cli_alias = "rm",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile's name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = ProfileDeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
