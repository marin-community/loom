use super::prelude::*;

/// Acknowledge a channel through a sequence number.
#[operation(
    id = "channels.read_marker.set",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/channels/write@v1"],
    cli = "channels ack",
    mcp = "loom_channel::ack",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// Mark read through this sequence; omission advances through the
    /// latest message.
    pub seq: Option<i64>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ChannelSubscriptionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
