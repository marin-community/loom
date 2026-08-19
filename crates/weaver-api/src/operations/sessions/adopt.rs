use super::prelude::*;

/// Rejoin an orphaned session to the active fleet: recreate its terminal (or
/// resume its ACP runtime) in place, without touching the worktree or branch.
#[operation(
    id = "sessions.adopt",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions adopt",
)]
pub struct Adopt;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
