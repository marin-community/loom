use super::prelude::*;

/// Subscribe to an ACP session's assistant token deltas.
///
/// The journaled counterpart is [`super::Chat`]; this is the same conversation
/// arriving a token at a time. Only ACP sessions have one — a terminal session
/// has no token stream, and the handler says so rather than parking the
/// connection.
#[operation(
    id = "sessions.chat.stream",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    io = Stream,
)]
pub struct Stream;

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
