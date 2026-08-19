use super::prelude::*;

/// Set (or replace) the default group a newly created session lands in for
/// one selector.
#[operation(
    id = "session_layout.defaults.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Set;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which kind of selector this default matches on.
    #[operand(json)]
    pub selector_kind: SessionPlacementSelectorKind,
    pub selector_value: String,
    pub group_id: String,
    /// Optimistic-concurrency guard: the layout revision this call was
    /// composed against. A stale caller is rejected rather than silently
    /// clobbering a concurrent edit from another dashboard tab.
    pub expected_revision: i64,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            selector_kind: SessionPlacementSelectorKind::Origin,
            selector_value: String::new(),
            group_id: String::new(),
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
