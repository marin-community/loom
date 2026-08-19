use super::prelude::*;

/// List named launch profiles. Secret environment values are never
/// returned.
#[operation(
    id = "profiles.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles list",
    cli_alias = "ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<ProfileView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
