use super::prelude::*;

/// Change an ACP session's permission mode (`session/set_mode`), journaling a
/// `mode_change` block.
#[operation(
    id = "sessions.mode",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions mode",
)]
pub struct Mode;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The mode id to switch to, as advertised by the adapter's metadata.
    #[operand(positional)]
    pub mode_id: String,
    /// Who is changing it (a watch name, or blank for `manual`).
    pub by: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionModeResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
