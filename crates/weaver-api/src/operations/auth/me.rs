use super::prelude::*;

/// Who the caller is, and which sign-in methods the server offers.
#[operation(
    id = "auth.me",
    actor = Anonymous,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth whoami",
)]
pub struct Me;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = MeView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
