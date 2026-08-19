//! Per-operator UI preferences (terminal theme/font/size) — a small,
//! fixed-key personal override layered over the effective inherited value.
//!
//! Distinct from `settings.*`: those are server-wide runtime configuration
//! with an admin-gated write (`settings.patch` is `actor = Admin`). This is
//! read/write by any signed-in human about their own account only.
//!
//! `preferences.patch` is deliberately not registered here. The legacy
//! `PATCH /preferences` body is an arbitrary `key -> string|number|null` map
//! (`Json<serde_json::Map<String, serde_json::Value>>`), which the `Operands`
//! derive cannot express losslessly: the only escape is narrowing every value
//! to a caller-stringified `Option<String>`, the trade `settings.patch`
//! already made for the same shape — and which broke at least one caller
//! that sent a raw JSON number/bool rather than a string. See the
//! `/preferences` entry in `crates/loom/tests/surface_parity.rs`'s
//! `UNREGISTERED_ROUTES`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod get;

static OPERATIONS: &[&OperationSpec] = &[<get::Get as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "preferences",
        label: "Operator preferences",
        operations: OPERATIONS,
    }
}
