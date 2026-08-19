use super::prelude::*;

/// Close one of a session's worktree debug shells, killing its supervisor.
#[operation(
    id = "sessions.shells.delete",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which of the session's debug shells to close.
    #[operand(positional)]
    pub index: u32,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// The shell indices still live after the close, so a client refreshes its tabs
/// in one round trip.
pub type Output = Vec<u32>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
