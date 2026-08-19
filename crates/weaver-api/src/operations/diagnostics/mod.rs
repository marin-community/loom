//! Operational health: the aggregated fleet snapshot (Settings →
//! Diagnostics) and build/process identity, for a human operator's debug
//! panel.
//!
//! Deliberately its own bundle rather than folding into `logs`: that bundle
//! is the log *text* itself (a snapshot and a live tail of the same lines);
//! this one is structured system state — session/profile counts, automation
//! run health, migration versions, build identity — a different shape of
//! data with no lines to redact. Both back the same Settings → Diagnostics
//! page, but grouping by what the data *is* keeps each bundle's operation
//! list legible rather than merging everything the page happens to render.

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
