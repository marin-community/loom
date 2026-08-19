//! The standalone operator shell.
//!
//! A terminal on the loom host itself (not attached to any session), requiring
//! `actor = Admin`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod restart;
pub mod terminal;

static OPERATIONS: &[&OperationSpec] = &[
    <restart::Restart as Operation>::SPEC,
    <terminal::Terminal as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "shell",
        label: "Operator shell",
        operations: OPERATIONS,
    }
}
