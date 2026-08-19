use super::prelude::*;

/// Atomically move one or more sessions to an exact insertion point within a
/// group.
#[operation(
    id = "session_layout.move",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Move;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    pub session_ids: Vec<String>,
    pub destination_group_id: String,
    /// Insert before this session in the destination group; omitted appends
    /// to the end.
    pub before_session_id: Option<String>,
    /// Optimistic-concurrency guard: the layout revision this call was
    /// composed against. Stale calls are rejected to prevent concurrent
    /// edit conflicts.
    pub expected_revision: i64,
}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
