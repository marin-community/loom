use super::prelude::*;

/// Deliver a new prompt to a session.
#[operation(
    id = "sessions.send",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions send",
)]
pub struct Send;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The text to type into the agent's pane.
    #[operand(positional)]
    pub text: String,
    /// Whether to follow the text with Enter to submit it as a turn. Omit for
    /// the default (submit); pass `false` to stage input unsubmitted.
    pub submit: Option<bool>,
    /// Who is sending (a watch name, or blank for `manual`).
    pub by: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionSendResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
