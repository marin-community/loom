use super::prelude::*;

/// Re-fetch the pull request currently associated with a session (by
/// explicit mapping, or by automatic current-open-PR discovery) and refresh
/// its cached status.
#[operation(
    id = "sessions.github.refresh",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
pub struct Refresh;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
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
