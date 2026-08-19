//! Operational health: the aggregated fleet snapshot (Settings →
//! Diagnostics) and build/process identity, for a human operator's debug
//! panel.
//!
//! Structured system state (session/profile counts, automation run health,
//! migration versions, build identity) — distinct from the `logs` bundle,
//! which is the log text itself. Both back the same Settings → Diagnostics
//! page.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod get;
pub mod status;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <status::Status as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "diagnostics",
        label: "Diagnostics",
        operations: OPERATIONS,
    }
}
