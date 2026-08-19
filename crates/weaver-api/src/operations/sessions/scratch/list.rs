use super::prelude::*;

/// List a session's Scratch files.
#[operation(
    id = "sessions.scratch.list",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions scratch list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = Vec<ScratchFileView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
