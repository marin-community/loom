use super::prelude::*;

/// Raw bytes of a worktree file (base64-encoded), with a guessed content type
/// — for inline image previews and downloads. Always reads the working tree,
/// never a git ref.
#[operation(
    id = "sessions.raw",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions raw",
)]
pub struct Raw;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Worktree-relative path to read.
    #[operand(positional)]
    pub path: String,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionRawFileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
