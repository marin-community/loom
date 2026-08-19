use super::prelude::*;
use serde_json::Value;
use std::collections::BTreeMap;

/// Set or clear this operator's personal UI preferences.
///
/// A `null` value clears the key back to the value it inherits. Unknown keys
/// are rejected per key with the whole patch refused, so a typo cannot half
/// apply.
///
/// `actor = User`: personal, not server-wide — every human writes their own
/// row and no session credential reaches the bundle at all. That is the
/// difference from `settings.patch`, which is `Admin`.
#[operation(
    id = "preferences.patch",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Patch;

/// `changes` is `key -> value`, where the value is any JSON scalar a caller
/// would naturally write (`"dark"`, `13`, `null`) rather than a pre-stringified
/// one. The server reduces it to the string the row stores — the same
/// coercion `settings.patch` does, and the same the legacy route did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(json, default = BTreeMap::new())]
    pub changes: BTreeMap<String, Option<Value>>,
}

pub type Output = UserPreferencesEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
