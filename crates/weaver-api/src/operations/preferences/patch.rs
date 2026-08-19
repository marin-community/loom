use super::prelude::*;
use serde_json::Value;
use std::collections::BTreeMap;

/// Set or clear this operator's personal UI preferences.
///
/// A `null` value clears the key back to the value it inherits. Unknown keys
/// are rejected per key with the whole patch refused, so a typo cannot half
/// apply.
#[operation(
    id = "preferences.patch",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Patch;

/// `changes` is `key -> value`, where the value is any JSON scalar a caller
/// would naturally write (`"dark"`, `13`, `null`). The server reduces it to
/// the string the row stores.
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
