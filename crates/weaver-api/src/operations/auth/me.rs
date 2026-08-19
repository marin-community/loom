use super::prelude::*;

/// Who the caller is, and which sign-in methods the server offers.
///
/// Previously excluded from the registry as an "administrative" endpoint even
/// though it is the opposite: every signed-in identity, and the login screen
/// itself, reads its own state through this operation.
///
/// `Anonymous` because the login screen calls it *before* there is a
/// credential, to discover which sign-in methods to offer. That is not a
/// widening: the hand-written `GET /api/auth/me` this replaces was already
/// mounted on the public router and already answered without one. The response
/// carries the caller's own identity and two booleans saying whether password
/// and GitHub sign-in are configured — nothing an unauthenticated caller could
/// not learn by looking at the login page.
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
