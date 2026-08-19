use super::prelude::*;

/// Read a channel's message history, advancing the read marker unless
/// peeking.
#[operation(
    id = "channels.messages.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/channels/read@v1"],
    cli = "channels read",
    mcp = "loom_channel::read",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// Only return items after this sequence number.
    #[operand(default = 0)]
    pub after: i64,
    /// Maximum number of items to return.
    #[operand(default = 100)]
    pub limit: i64,
    /// Restrict to these message kinds (`goal`, `message`, `status`,
    /// `result`, `system`).
    pub kinds: Vec<String>,
    /// Read without advancing this session's read marker.
    #[operand(default = false)]
    pub peek: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ChannelMessageView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
