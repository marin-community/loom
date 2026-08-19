use super::prelude::*;

/// The signed-in operator's shared session-dashboard layout: spaces, groups,
/// session placements, and per-selector placement defaults.
#[operation(
    id = "session_layout.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = SessionLayoutView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
