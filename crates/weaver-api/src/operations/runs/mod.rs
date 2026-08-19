//! Automation-triggered runs — GitHub Actions, ops scripts, and Grafana
//! alerts dispatching a session through a federated automation credential.
//!
//! A `weaver_core::runs::Run` is one delivery attempt through
//! `POST /api/auth/automation-token`-minted credentials, tracked for
//! idempotent redelivery and operator observability. `runs.create` is gated
//! to the runtime (`actor = Internal`), while the read side is an operator
//! diagnostic surface (`actor = User`).

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod create;
pub mod get;
pub mod list;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "runs",
        label: "Automation runs",
        operations: OPERATIONS,
    }
}
