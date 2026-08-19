use super::prelude::*;

/// Set or change the caller's own password.
#[operation(
    id = "auth.set_password",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth password",
)]
pub struct SetPassword;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The new password (minimum 8 characters).
    pub new_password: String,
}

pub type Output = UserView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
