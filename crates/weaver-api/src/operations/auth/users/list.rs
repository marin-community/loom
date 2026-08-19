use super::prelude::*;

/// List the approved operators.
#[operation(
    id = "auth.users.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth users ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<UserView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
