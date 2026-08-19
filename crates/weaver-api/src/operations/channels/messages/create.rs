use super::prelude::*;

/// Append and deliver a durable channel message.
///
/// Idempotent on `idempotency_key`.
#[operation(
    id = "channels.messages.create",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/channels/write@v1"],
    cli = "channels send",
    mcp = "loom_channel::send",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible channel id. Empty means this session's own channel,
    /// resolved server-side.
    #[operand(default = String::new())]
    pub channel: String,
    /// The message body.
    #[operand(positional)]
    pub body: String,
    /// `message`, `status`, or `result`.
    #[operand(default = String::from("message"))]
    pub kind: String,
    /// `normal`, `attention`, or `blocked`.
    #[operand(default = String::from("normal"))]
    pub urgency: String,
    /// Arbitrary structured payload alongside the body.
    #[operand(json, default = serde_json::json!({}))]
    pub payload: serde_json::Value,
    /// Reply to an existing message in this channel.
    pub reply_to: Option<String>,
    /// Retry-safe key scoped to the channel.
    pub idempotency_key: Option<String>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ChannelMessageView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
