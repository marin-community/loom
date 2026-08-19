use super::prelude::*;

/// Update a session's branch-level fields (title, goal, description) and its
/// durable status. Attention level is managed via tags operations
/// (`sessions.tags.set`/`sessions.tags.delete`).
#[operation(
    id = "sessions.update",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// New durable status (the fleet lifecycle marker).
    pub status: Option<String>,
    /// New task label for the branch.
    pub title: Option<String>,
    /// Required with `title`: the label the caller last observed. Used to detect
    /// and reject concurrent updates by comparing with the current value.
    pub expected_title: Option<String>,
    /// Required with `title`: the provenance (`user` or `agent`) the caller
    /// last observed.
    pub expected_title_provenance: Option<String>,
    /// New goal text for the branch.
    pub goal: Option<String>,
    /// The agent's current-state message — the prose shown beside the
    /// attention level.
    pub description: Option<String>,
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
