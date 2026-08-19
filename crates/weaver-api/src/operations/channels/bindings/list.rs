use super::prelude::*;

/// List a channel's external delivery bindings: subscribed session inboxes,
/// plus the originating Slack thread if the branch is wired to one.
#[operation(
    id = "channels.bindings.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/channels/read@v1"],
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<ChannelBindingView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
