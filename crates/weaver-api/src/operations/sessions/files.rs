use super::prelude::*;

/// Worktree file completion for the chat composer: tracked plus unignored
/// untracked paths, optionally filtered by a case-insensitive substring.
#[operation(
    id = "sessions.files",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions files",
)]
pub struct Files;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Case-insensitive substring filter. Blank matches everything.
    #[operand(default = String::new())]
    pub q: String,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionFilesView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
