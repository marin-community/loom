use super::prelude::*;

/// Clear a placement default, so newly created sessions matching this
/// selector fall through to a broader default (or the fallback origin `*`,
/// which cannot itself be removed).
///
/// The legacy `{kind}/{value}` route names two ordinary path parameters —
/// neither is the caller's own session, branch, or repo — so both are plain
/// operands rather than `#[operand(context)]`.
#[operation(
    id = "session_layout.defaults.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Delete;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which kind of selector the default to clear matches on.
    #[operand(json)]
    pub selector_kind: SessionPlacementSelectorKind,
    pub selector_value: String,
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
