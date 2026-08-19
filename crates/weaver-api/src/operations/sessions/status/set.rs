use super::prelude::*;

/// Update the durable attention level and status message.
#[operation(
    id = "sessions.status.set",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "status set",
    mcp = "loom_session::status_set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The attention level: `ok`, `attention`, or `blocked`.
    #[operand(long = "tag")]
    pub level: String,
    /// The current-state message shown alongside the level.
    pub message: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = BranchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
