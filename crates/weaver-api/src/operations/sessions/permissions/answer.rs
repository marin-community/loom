use super::prelude::*;

/// Answer a pending in-flight ACP permission prompt by its chosen option:
/// 404 for an unknown request id, 409 when it was already resolved.
///
/// Human-only. Agents cannot resolve their own permission prompts.
#[operation(
    id = "sessions.permissions.answer",
    actor = User,
    scope = Session,
    risk = Write,
    grants = [],
)]
pub struct Answer;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The live permission request to answer.
    #[operand(positional)]
    pub request_id: String,
    /// The chosen option's id, as advertised by the prompt.
    pub option_id: String,
    /// Who is answering (a watch name, or blank for `manual`).
    pub by: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.permissions.answer`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AnswerPermissionResult {
    pub resolved: bool,
    pub option_id: String,
}

pub type Output = AnswerPermissionResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
