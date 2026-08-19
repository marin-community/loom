use super::prelude::*;

/// Open a custom durable channel.
///
/// Scoped to a repository (humans launching from the dashboard specify no branch).
/// A session's opening branch is recorded for provenance, not scope.
#[operation(
    id = "channels.create",
    actor = SessionSelf,
    scope = Repository,
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
    /// The repository the channel belongs to. Resolved from the calling
    /// session when it has one.
    #[operand(context)]
    pub repo_root: String,
    /// The branch that opened the channel, for provenance. Resolved from the
    /// calling session; a human launch leaves it unset.
    #[operand(context)]
    pub branch: Option<String>,
}

pub type Output = ChannelView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
