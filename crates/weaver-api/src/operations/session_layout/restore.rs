use super::prelude::*;

/// Atomically restore the complete membership and order of a set of groups.
///
/// The supplied groups must cover exactly the sessions currently placed in
/// those groups, so an undo fails as a stale whole instead of partially
/// overwriting an intervening placement.
#[operation(
    id = "session_layout.restore",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Restore;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(json)]
    pub groups: Vec<SessionGroupOrderReq>,
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
