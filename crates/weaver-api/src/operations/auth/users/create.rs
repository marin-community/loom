use super::prelude::*;

/// Approve a new operator, same reasoning as [`list`](super::list).
#[operation(
    id = "auth.users.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth users add",
)]
pub struct Create;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(positional)]
    pub username: String,
    /// The GitHub login allowed to sign in as this operator.
    pub github_login: Option<String>,
    /// A password, if this operator should also be able to sign in with one.
    /// At least one of `github_login` or `password` is required.
    pub password: Option<String>,
    /// `admin` or `user`.
    #[operand(json, default = UserRole::User)]
    pub role: UserRole,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            username: String::new(),
            github_login: None,
            password: None,
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
