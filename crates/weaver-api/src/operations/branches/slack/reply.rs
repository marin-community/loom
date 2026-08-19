use super::prelude::*;

/// Post a message from this branch's session back to a Slack thread it owns.
///
/// The bot token stays server-side: the agent (holding `LOOM_TOKEN`) calls
/// this route rather than being handed the workspace-wide credential — the
/// Slack analog of a session replying with `gh`. Without `thread`, the
/// branch's own `slack` wiring tag is used (the conversation the session was
/// born from); with `thread`, the reply targets one of the threads an
/// automation delivery routed to this branch.
#[operation(
    id = "branches.slack.reply",
    actor = SessionSelf,
    scope = Branch,
    risk = ExternalWrite,
    grants = ["loom/branches/write@v1"],
    cli = "branches slack reply",
    mcp = "loom_messaging::slack_reply",
)]
pub struct Reply;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The message text.
    #[operand(positional)]
    pub text: String,
    /// Reply in a specific delivered thread instead of the branch's own Slack
    /// wiring. On the command line this takes a JSON object, because a thread
    /// reference is not a flag.
    #[operand(json, default = None)]
    pub thread: Option<SlackThreadRef>,
    /// Dedupe key so a retried send doesn't double-post.
    pub idempotency_key: Option<String>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

/// The response shape differs by destination (the branch's own wiring
/// reports a delivery record; an explicit `thread` reports a bare `ts`), so
/// this stays untyped JSON rather than force one shape onto both — matching
/// `sessions.interrupt`/`sessions.send`.
pub type Output = serde_json::Value;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
