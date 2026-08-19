use super::prelude::*;

/// Mint a new personal API token. The plaintext is returned once — the
/// server keeps only a hash.
#[operation(
    id = "auth.tokens.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "token add",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A label to recognise the token by (e.g. `github-actions`).
    #[operand(positional)]
    pub name: String,
    /// Optional lifetime in days; omitted or non-positive never expires.
    pub expires_in_days: Option<i64>,
}

pub type Output = CreatedTokenView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
