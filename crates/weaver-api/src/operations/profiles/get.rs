use super::prelude::*;

/// Show one named launch profile. Secret environment values are never
/// returned.
#[operation(
    id = "profiles.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile's name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = ProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
