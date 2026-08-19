use super::prelude::*;

/// Revoke one of the caller's own personal API tokens.
#[operation(
    id = "auth.tokens.revoke",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "token rm",
)]
pub struct Revoke;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The token id (from `token ls`).
    #[operand(positional)]
    pub id: String,
}

pub type Output = RevokeTokenResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
