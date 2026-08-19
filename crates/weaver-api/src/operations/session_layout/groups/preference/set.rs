use super::prelude::*;

/// Set whether one group is collapsed in the caller's own dashboard.
///
/// Unlike its bundle siblings this carries no `expected_revision`: it is a
/// per-operator disclosure preference (`user_session_group_state`), not
/// shared layout state another dashboard tab could race to change, so there
/// is nothing to guard against.
#[operation(
    id = "session_layout.groups.preference.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The group whose disclosure state is being set.
    pub id: String,
    pub collapsed: bool,
}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
