use super::prelude::*;

/// Delete a group. Deleting a group never deletes sessions:
/// `destination_group_id` is required whenever the group owns placements or
/// default-placement selectors, and its contents move there atomically.
#[operation(
    id = "session_layout.groups.delete",
    actor = User,
    scope = Global,
    risk = Destructive,
    grants = [],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The group being deleted.
    pub id: String,
    /// Where the group's sessions and placement defaults land. Required
    /// unless the group is empty.
    pub destination_group_id: Option<String>,
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
