use super::prelude::*;

/// Delete one Scratch file.
#[operation(
    id = "sessions.scratch.delete",
    actor = SessionSelf,
    scope = Session,
    risk = Destructive,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions scratch delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The file name to delete.
    #[operand(positional)]
    pub name: String,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = ScratchDeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
