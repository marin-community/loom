use super::prelude::*;

/// Attach to a session's agent terminal over a websocket.
///
/// `io = Duplex` because the response is a protocol upgrade served by a custom
/// handler. Registering it here declares the actor policy, resource scope, and
/// operands explicitly.
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
