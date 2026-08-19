use super::prelude::*;

/// Read a bounded terminal preview.
#[operation(
    id = "sessions.preview",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions preview",
)]
pub struct Preview;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Extra scrollback lines to include above the visible screen (0 = just
    /// the visible pane).
    #[operand(default = 0)]
    pub lines: i64,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionPreviewResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
