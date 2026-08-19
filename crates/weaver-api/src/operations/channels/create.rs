use super::prelude::*;

/// Open a custom durable channel.
#[operation(
    id = "channels.create",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/channels/write@v1"],
    cli = "channels open",
    mcp = "loom_channel::open",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The new channel's name.
    #[operand(positional)]
    pub name: String,
    /// Optional topic description.
    #[operand(default = String::new())]
    pub topic: String,
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
