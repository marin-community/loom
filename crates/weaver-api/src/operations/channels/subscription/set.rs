use super::prelude::*;

/// Set how a session follows a channel.
#[operation(
    id = "channels.subscription.set",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/channels/write@v1"],
    cli = "channels subscribe",
    mcp = "loom_channel::subscribe",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// `observe` or `deliver`.
    #[operand(default = String::from("observe"))]
    pub mode: String,
    /// Subscribe this descendant session instead of the caller.
    pub session: Option<String>,
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
