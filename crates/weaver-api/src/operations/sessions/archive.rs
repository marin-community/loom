use super::prelude::*;

/// Archive a session: tear down its terminal and worktree, keeping the branch,
/// its commits, the session row, and run history. The inverse of `recover`.
#[operation(
    id = "sessions.archive",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions archive",
)]
pub struct Archive;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionArchiveResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
