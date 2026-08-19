//! Declarative reconciliation of runtime settings, launch profiles, and
//! workload federation mappings against one deployment stack.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod reconcile;

static OPERATIONS: &[&OperationSpec] = &[<reconcile::Reconcile as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "deployment",
        label: "Deployment reconciliation",
        operations: OPERATIONS,
    }
}
