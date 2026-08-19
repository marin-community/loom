use super::prelude::*;

/// Create a new group within a space.
#[operation(
    id = "session_layout.groups.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    pub space_id: String,
    pub name: String,
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
