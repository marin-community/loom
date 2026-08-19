use super::prelude::*;

/// Subscribe to an ACP session's assistant token deltas.
///
/// Available only for ACP sessions. Terminal sessions have no token stream.
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
