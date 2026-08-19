//! Per-operator UI preferences (terminal theme/font/size) — a small,
//! fixed-key personal override layered over the effective inherited value.
//!
//! Distinct from `settings.*`: those are server-wide runtime configuration
//! with an admin-gated write (`settings.patch` is `actor = Admin`). This is
//! read/write by any signed-in human about their own account only.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod get;
pub mod patch;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <patch::Patch as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "preferences",
        label: "Operator preferences",
        operations: OPERATIONS,
    }
}
