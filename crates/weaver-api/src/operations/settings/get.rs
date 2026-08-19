use super::prelude::*;

/// Every registered runtime setting and its effective value.
#[operation(
    id = "settings.get",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "settings get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = SettingsEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
