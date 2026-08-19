use super::prelude::*;

/// Resolve this caller's session, branch, repository, channel, and links.
#[operation(
    id = "self.get",
    bundle = "sessions",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "self",
    mcp = "loom_context::get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = SelfContextView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
