use super::prelude::*;

/// Recover an archived session: rebuild its worktree from the kept branch, then
/// resume the agent. For a live (non-archived) session, restart its ACP
/// runtime instead. The inverse of `archive`.
#[operation(
    id = "sessions.recover",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions recover",
)]
pub struct Recover;

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
