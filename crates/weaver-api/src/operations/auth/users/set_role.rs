use super::prelude::*;

/// Change an operator's role. Existing cookies and personal tokens observe
/// the change on their next request.
#[operation(
    id = "auth.users.set_role",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth users role",
)]
pub struct SetRole;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(positional)]
    pub username: String,
    /// `admin` or `user`.
    #[operand(json)]
    pub role: UserRole,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            username: String::new(),
            role: UserRole::User,
        }
    }
}

pub type Output = UserView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
