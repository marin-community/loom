use std::collections::BTreeMap;

use super::prelude::*;

/// Apply setting changes. A `null` value clears a key back to its default.
#[operation(
    id = "settings.patch",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings patch",
)]
pub struct Patch;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Dotted setting key to new value; `null` clears that key back to its
    /// default.
    #[operand(json, default = BTreeMap::new())]
    pub changes: BTreeMap<String, Option<String>>,
}

pub type Output = SettingsEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
