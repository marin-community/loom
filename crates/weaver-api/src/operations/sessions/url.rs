use super::prelude::*;

/// The externally-visible dashboard URL for a session. The agent inside a
/// session only knows its loopback API address, so only the server can resolve
/// this — from the configured `auth.base_url`, or the address it is bound to.
#[operation(
    id = "sessions.url",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions url",
)]
pub struct Url;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionUrlView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
