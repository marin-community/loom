use super::prelude::*;

/// Wait for the next matching channel message.
#[operation(
    id = "channels.wait",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/channels/read@v1"],
    cli = "channels wait",
    mcp = "loom_channel::wait",
    view = View,
)]
pub struct Wait;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// Wait for items after this sequence; omission starts from the
    /// channel's latest known message.
    pub after: Option<i64>,
    /// Wake only for this message kind, e.g. `result`.
    pub kind: Option<String>,
    /// Wake only for `attention` or `blocked` urgency.
    #[operand(default = false)]
    pub urgent: bool,
    /// Seconds to wait before giving up.
    #[operand(default = 1800)]
    pub timeout: i64,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ChannelMessageView;

/// CLI-only flags that never cross the wire.
#[derive(Debug, Clone, Default, View)]
pub struct View {
    /// Seconds between polls while waiting.
    #[operand(default = 2)]
    pub interval: i64,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
