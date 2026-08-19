use super::prelude::*;

/// Attach to one of a session's worktree debug shells over a websocket.
///
/// Unlike [`super::super::terminal::Terminal`] the target is a plain login
/// shell *in the session's worktree*, not the agent, and it is spawned on first
/// attach — which is why this is `risk = ExternalWrite`: it runs arbitrary
/// commands as the operator inside the session's checkout.
#[operation(
    id = "sessions.shells.terminal",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/sessions/write@v1"],
    io = Duplex,
)]
pub struct Terminal;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which of the session's debug shells; several may run at once.
    #[serde(default)]
    #[operand(default = 0u32)]
    pub index: u32,
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
