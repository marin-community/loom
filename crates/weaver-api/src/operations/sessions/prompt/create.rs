use super::prelude::*;

/// Send a user message to an ACP session. Dispatched immediately when idle,
/// or appended to the durable queue while a turn is live; `send_now` instead
/// cancels any live turn and starts the message as a normal prompt. Every
/// send records a `nudge` event on the branch (the audit rule).
///
/// The legacy body accepted a caller-supplied `by`. This operation does not:
/// provenance is derived from the credential, the same way
/// `issues.tags.set` derives its `by` rather than trusting whatever a caller
/// names itself — `manual` for a human operator, `agent` otherwise.
#[operation(
    id = "sessions.prompt.create",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The message text.
    #[operand(positional)]
    pub text: String,
    /// Cancel any live turn and start this message as a normal prompt.
    #[operand(default = false)]
    pub send_now: bool,
    /// Promote the server's durable next-turn queue instead of sending
    /// `text`. Keeps the action race-free when a client is showing queued
    /// copy.
    #[operand(default = false)]
    pub force_queued: bool,
    /// Worktree-relative files to attach as ACP resource links.
    #[serde(default)]
    pub files: Vec<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.prompt.create`. Mirrors the ACP task's own
/// acknowledgement (`queued`, `turn`), the same shape `sessions.send` returns
/// for an ACP session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PromptResult {
    pub queued: bool,
    pub turn: Option<i64>,
}

pub type Output = PromptResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
