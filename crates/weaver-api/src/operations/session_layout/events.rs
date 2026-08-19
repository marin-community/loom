use super::prelude::*;

/// Subscribe to layout changes as other dashboard tabs make them.
///
/// `actor = User`: the layout is the signed-in operator's own dashboard state,
/// and a session credential has never been able to read it.
#[operation(
    id = "session_layout.events",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    io = Stream,
)]
pub struct Events;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
