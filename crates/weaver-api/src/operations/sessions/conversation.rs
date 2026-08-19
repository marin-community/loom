use super::prelude::*;

// `sessions.conversation.block` serves the untruncated content one elided
// block points at, and lives in `conversation/block.rs`, so the id keeps
// naming the file.
pub(super) use super::prelude;
pub mod block;

/// The session's agent conversation as a normalized iris log — the live
/// transcript when present, else the capture archived alongside it. Oversized
/// tool payloads are elided to a preview naming `sessions.conversation.block`
/// and the coordinates that fetch the rest.
#[operation(
    id = "sessions.conversation",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions conversation",
)]
pub struct Conversation;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = weaver_core::transcript::iris::Log;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
