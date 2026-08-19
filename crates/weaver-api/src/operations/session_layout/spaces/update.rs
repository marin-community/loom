use super::prelude::*;

/// Rename a space.
#[operation(
    id = "session_layout.spaces.update",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The space being renamed.
    pub id: String,
    pub name: String,
    /// Optimistic-concurrency guard: the layout revision this call was
    /// composed against. Rejects stale callers to prevent clobbering concurrent
    /// edits from other dashboard tabs.
    pub expected_revision: i64,
}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
