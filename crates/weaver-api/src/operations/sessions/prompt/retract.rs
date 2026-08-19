use super::prelude::*;

/// Pull unseen next-turn feedback back out of the durable queue for editing.
/// The ACP task owns the consume so this action is serialized with automatic
/// dispatch at a turn boundary.
#[operation(
    id = "sessions.prompt.retract",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Retract;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.prompt.retract`: the retracted text.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RetractResult {
    pub text: String,
}

pub type Output = RetractResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
