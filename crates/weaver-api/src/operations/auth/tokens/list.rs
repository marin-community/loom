use super::prelude::*;

/// List the caller's own personal API tokens (metadata only — never the
/// secret, see [`create`](super::create)).
#[operation(
    id = "auth.tokens.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "token ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<TokenView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
