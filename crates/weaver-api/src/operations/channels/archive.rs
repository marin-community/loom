use super::prelude::*;

/// Archive a custom channel.
///
/// Only a custom channel: a session's own channel follows the session's
/// lifecycle, and archiving it out from under the session is refused. Who may
/// archive is narrower than who may reach the channel, so the handler still
/// checks it — a non-human credential may archive only what it opened.
#[operation(
    id = "channels.archive",
    actor = SessionSelf,
    scope = Branch,
    risk = Destructive,
    grants = ["loom/channels/write@v1"],
    cli = "channels archive",
)]
pub struct Archive;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id.
    #[operand(positional)]
    pub channel: String,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ChannelArchiveResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
