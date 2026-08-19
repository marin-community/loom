use std::collections::BTreeMap;

use serde_json::Value;

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
    ///
    /// A value may be a string, a boolean, or a number. Settings are *stored* as
    /// strings, but a caller naturally writes the setting's own type — `false`
    /// for `auth.trust_loopback`, `300` for a `_secs` key — and requiring
    /// `"false"` would make this operation stricter than the route it replaces
    /// for no benefit. Coercion happens once, server-side; anything else (an
    /// array, an object) is rejected by key.
    #[operand(json, default = BTreeMap::new())]
    pub changes: BTreeMap<String, Option<Value>>,
}

pub type Output = SettingsEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
