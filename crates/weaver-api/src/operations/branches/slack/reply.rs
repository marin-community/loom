use super::prelude::*;

/// Post a message from this branch's session back to a Slack thread.
///
/// Without `thread`, replies to the branch's own Slack wiring; with `thread`,
/// targets a delivered thread.
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
    /// Delivered thread to reply in (optional).
    #[operand(json, default = None)]
    pub thread: Option<SlackThreadRef>,
    /// Dedupe key so a retried send doesn't double-post.
    pub idempotency_key: Option<String>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = serde_json::Value;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
