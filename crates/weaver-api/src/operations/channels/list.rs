use super::prelude::*;

/// List visible durable channels and their unread state.
#[operation(
    id = "channels.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/channels/read@v1"],
    cli = "channels list",
    cli_alias = "ls",
    mcp = "loom_channel::list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Include archived channels.
    #[operand(default = false)]
    pub archived: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ChannelView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
