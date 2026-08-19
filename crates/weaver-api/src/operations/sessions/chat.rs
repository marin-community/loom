use super::prelude::*;

// `sessions.chat.stream` is the live half of this operation and lives in
// `chat/stream.rs`, so the id keeps naming the file.
pub(super) use super::prelude;
pub mod stream;

/// The journaled ACP conversation plus the agent-owned composer metadata,
/// paged newest-first.
#[operation(
    id = "sessions.chat",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions chat",
)]
pub struct Chat;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Page before this turn (paired with `before_seq`).
    pub before_turn: Option<i64>,
    /// Page before this sequence number within `before_turn`.
    pub before_seq: Option<i64>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionChatView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
