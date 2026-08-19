//! The standalone operator shell.
//!
//! Not attached to any session: a terminal on the loom host itself, which is
//! why it is `actor = Admin`. It was already the most privileged thing on the
//! HTTP surface; now that is a declaration rather than an entry in a
//! hand-maintained path denylist.

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
