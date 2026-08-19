use super::prelude::*;

/// Pin a session's branch to an explicit pull request and fetch it
/// immediately. The mapping is persisted only after GitHub confirms the
/// number, so a typo never replaces a working association with a dead one.
#[operation(
    id = "sessions.github.set",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The pull request number to pin to.
    #[operand(positional)]
    pub pr_number: i64,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
