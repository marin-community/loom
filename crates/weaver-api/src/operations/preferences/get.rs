use super::prelude::*;

/// Get this operator's personal UI preference overrides (terminal theme, font,
/// font size), each layered over its effective inherited value.
#[operation(
    id = "preferences.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = UserPreferencesEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
