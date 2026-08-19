use super::prelude::*;

/// Exchange a username and password for a signed-in session.
///
/// `actor = Anonymous`: this is the operation that *creates* a credential, so it
/// necessarily runs before one exists. It is one of exactly two such operations,
/// and `anonymous_operations_are_pinned` asserts that set.
/// `loom login` (the CLI) reaches the API too, but by verifying a
/// pre-existing personal token against [`auth.me`](super::me) and
/// [`auth.tokens.list`](super::tokens::list), not by calling this: storing
/// the resulting credential in a local client context is Tier C and stays
/// hand-written, so this operation has no `cli` projection.
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
