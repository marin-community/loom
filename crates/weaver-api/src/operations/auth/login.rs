use super::prelude::*;

/// Exchange a username and password for a signed-in session.
///
/// `actor = Anonymous`: this is one of exactly two operations that create a
/// credential, running before any credential exists. This is asserted by
/// `anonymous_operations_are_pinned`.
///
/// The CLI tool implements `loom login` separately by verifying a personal
/// token against [`auth.me`](super::me) and [`auth.tokens.list`](super::tokens::list),
/// then storing it locally. That's a hand-written Tier C client feature, so
/// this operation has no `cli` projection.
#[operation(
    id = "auth.login",
    actor = Anonymous,
    io = Session,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Login;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    pub username: String,
    pub password: String,
}

pub type Output = MeView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
