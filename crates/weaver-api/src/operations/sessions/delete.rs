use super::prelude::*;

/// Fully remove a session: tear down its terminal/worktree and, unless
/// `keep_branch` is set, the branch and its commits too. The session row and
/// run history are removed as well. This is irreversible; see `sessions.archive`
/// to keep session data.
#[operation(
    id = "sessions.delete",
    actor = SessionSelf,
    scope = Session,
    risk = Destructive,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Keep the branch (and its commits) instead of deleting it along with
    /// the session.
    #[operand(default = false)]
    pub keep_branch: bool,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.delete`. `kind` is `"session"` for a real session or
/// `"launch_attempt"` when the id named a reservation that never became one,
/// mirroring [`super::archive::Archive`]'s result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DeleteResult {
    pub deleted: bool,
    pub kind: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub type Output = DeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
