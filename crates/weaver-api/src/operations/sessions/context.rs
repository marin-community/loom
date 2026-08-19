use super::prelude::*;

/// Resolve this caller's session, branch, repository, channel, and links.
#[operation(
    // `self` cannot be a Rust module name — not even as a raw identifier — so an
    // id of `self.get` could never live in the file its name promises. The CLI
    // still spells it `loom context` and MCP still calls it `loom_context::get`:
    // projections are named independently of identity, which is the point.
    id = "sessions.context",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "context",
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
