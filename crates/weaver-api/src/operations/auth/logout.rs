use super::prelude::*;

/// End the caller's signed-in session.
///
/// The session to drop is read from the request's cookie, not the body —
/// same pattern as a `SessionSelf` operation resolving `session` from
/// context, except the source is the transport rather than the dispatcher.
#[operation(
    id = "auth.logout",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Logout;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

/// The caller's identity after logout (`authenticated: false`), so the
/// client learns the outcome without a follow-up `auth.me` call.
pub type Output = MeView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
