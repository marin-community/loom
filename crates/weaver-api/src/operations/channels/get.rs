use super::prelude::*;

/// Inspect one channel and its delivery bindings.
#[operation(
    id = "channels.get",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/channels/read@v1"],
    cli = "channels get",
    mcp = "loom_channel::get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(positional, default = String::new())]
    pub channel: String,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ChannelView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
