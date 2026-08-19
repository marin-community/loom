use super::prelude::*;

/// Reorder one space, or one group (optionally into another space).
#[operation(
    id = "session_layout.reorder",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Reorder;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Whether `id` names a space or a group.
    #[operand(json)]
    pub kind: SessionLayoutItemKind,
    /// The space or group being repositioned.
    pub id: String,
    /// Insert before this sibling; omitted moves to the end.
    pub before_id: Option<String>,
    /// For a group, move it into this space; omitted keeps its current space.
    pub destination_space_id: Option<String>,
    /// Optimistic-concurrency guard: the layout revision this call was
    /// composed against. Stale calls are rejected to prevent concurrent
    /// edit conflicts.
    pub expected_revision: i64,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            kind: SessionLayoutItemKind::Space,
            id: String::new(),
            before_id: None,
            destination_space_id: None,
            expected_revision: 0,
        }
    }
}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
