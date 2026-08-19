use super::prelude::*;

/// Remove one profile's write-only environment variable.
#[operation(
    id = "profiles.env.delete",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles env delete",
    cli_alias = "rm",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The owning profile's name.
    #[operand(positional)]
    pub profile: String,
    /// The variable name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = ProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
