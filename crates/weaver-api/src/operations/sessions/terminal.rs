use super::prelude::*;

/// Attach to a session's agent terminal over a websocket.
///
/// `io = Duplex`: the response is a protocol upgrade, so a custom handler
/// serves it. Registering it anyway is the point — the actor policy, the
/// resource scope, and the operand it takes are declared here instead of being
/// implied by a route string and a middleware allowlist.
///
/// `risk = Write` because this is a real PTY: whoever holds it types as the
/// agent's user.
#[operation(
    id = "sessions.terminal",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    io = Duplex,
)]
pub struct Terminal;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[serde(default)]
    #[operand(context)]
    pub session: String,
}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
