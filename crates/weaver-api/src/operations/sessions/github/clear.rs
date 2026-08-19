use super::prelude::*;

/// Clear an explicit PR mapping and return to automatic current-open-PR
/// discovery.
#[operation(
    id = "sessions.github.clear",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
pub struct Clear;

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
