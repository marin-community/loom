use super::prelude::*;

/// List every registered watch: name, enabled, trigger, program, last outcome.
///
/// Previously excluded from the registry as "fleet automation". This is the
/// operator + authoring surface over `weaver_core::watch` — human-readable by
/// any signed-in operator (`GET` is unconditionally allowed for a `User`
/// grant), but a session credential has never been able to reach `/watches`,
/// so this stays `actor = User` rather than `SessionSelf`.
#[operation(
    id = "watches.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<WatchView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
