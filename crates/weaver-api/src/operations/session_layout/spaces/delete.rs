use super::prelude::*;

/// Delete a space. Deleting a non-empty space atomically moves its sessions
/// and placement defaults to `destination_group_id`, which is required
/// unless the space is empty. The last remaining space cannot be deleted.
#[operation(
    id = "session_layout.spaces.delete",
    actor = User,
    scope = Global,
    risk = Destructive,
    grants = [],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The space being deleted.
    pub id: String,
    /// Where the space's sessions and placement defaults land. Required
    /// unless the space is empty.
    pub destination_group_id: Option<String>,
    /// Optimistic-concurrency guard: the layout revision this call was
    /// composed against. A stale caller is rejected rather than silently
    /// clobbering a concurrent edit from another dashboard tab.
    pub expected_revision: i64,
}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
