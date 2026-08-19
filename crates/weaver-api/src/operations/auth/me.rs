use super::prelude::*;

/// Who the caller is, and which sign-in methods the server offers.
///
/// Previously excluded from the registry as an "administrative" endpoint even
/// though it is the opposite: every signed-in identity, and the login screen
/// itself, reads its own state through this operation.
#[operation(
    id = "auth.me",
    actor = User,
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
