//! Per-operator UI preferences (terminal theme/font/size) — a small,
//! fixed-key personal override layered over the effective inherited value.
//!
//! Distinct from `settings.*`: those are server-wide runtime configuration
//! with an admin-gated write (`settings.patch` is `actor = Admin`). This is
//! read/write by any signed-in human about their own account only.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod get {
    use super::prelude::*;

    /// Get this operator's personal UI preference overrides (terminal theme, font,
    /// font size), each layered over its effective inherited value.
    #[operation(id = "preferences.get", actor = User, scope = Global, risk = Read)]
    pub struct Input {}

    pub type Output = UserPreferencesEnvelope;
}

pub mod patch {
    use super::prelude::*;
    use serde_json::Value;
    use std::collections::BTreeMap;

    /// Set or clear this operator's personal UI preferences.
    ///
    /// `changes` is `key -> value`, where the value is any JSON scalar a caller
    /// would naturally write (`"dark"`, `13`, `null`); the server reduces it to
    /// the string the row stores. A `null` value clears the key back to the value
    /// it inherits. Unknown keys are rejected per key with the whole patch
    /// refused, so a typo cannot half apply.
    #[operation(id = "preferences.patch", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        #[operand(json, default = BTreeMap::new())]
        pub changes: BTreeMap<String, Option<Value>>,
    }

    pub type Output = UserPreferencesEnvelope;
}

static OPERATIONS: &[&OperationSpec] = &[get::SPEC, patch::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "preferences",
        label: "Operator preferences",
        operations: OPERATIONS,
    }
}
