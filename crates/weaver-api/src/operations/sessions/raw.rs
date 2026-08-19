use super::prelude::*;

/// Raw bytes of a worktree file, with a guessed content type — for inline
/// image previews and downloads. Always reads the working tree, never a git ref.
///
/// `io = Download` because the browser fetches this resource directly and needs
/// raw bytes rather than a JSON envelope.
#[operation(
    id = "sessions.raw",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    io = Download,
)]
pub struct Raw;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Worktree-relative path to read.
    //
    // `serde(default)` because a download's operands arrive in the query string,
    // which axum extracts before any default-filling could run. The handler
    // rejects an empty path.
    #[serde(default)]
    #[operand(default = String::new())]
    pub path: String,
    /// A visible session id. Omit for this session.
    #[serde(default)]
    #[operand(context)]
    pub session: String,
}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
