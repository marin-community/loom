use super::prelude::*;

/// End the caller's signed-in session.
#[operation(
    id = "auth.logout",
    actor = User,
    io = Session,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Logout;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

/// The caller's identity after logout (`authenticated: false`).
pub type Output = MeView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
