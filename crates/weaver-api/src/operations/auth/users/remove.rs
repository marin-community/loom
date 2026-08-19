use super::prelude::*;

/// Remove an approved operator. A caller may not remove themself.
#[operation(
    id = "auth.users.remove",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "auth users rm",
)]
pub struct Remove;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(positional)]
    pub username: String,
}

pub type Output = RemoveUserResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
