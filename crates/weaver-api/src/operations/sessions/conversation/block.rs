use super::prelude::*;

/// One conversation block, untruncated — what the `full` pointer
/// `sessions.conversation` leaves in place of an oversized tool payload names.
/// Addressed by position in the log, matching [`super::Conversation`].
#[operation(
    id = "sessions.conversation.block",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
)]
pub struct Block;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which message in the conversation.
    #[operand(positional)]
    pub message: u32,
    /// Which block within that message.
    pub block: u32,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = weaver_core::transcript::iris::Block;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
