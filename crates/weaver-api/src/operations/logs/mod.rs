//! The server log text: a snapshot (`list`) and a live tail (`stream`).
//!
//! Human-only diagnostics (Settings → Diagnostics). Redaction is applied per
//! subscriber from the caller's own credential, which is why the stream
//! cannot be a shared broadcast served without a principal. The structured
//! half of the same page — the fleet health snapshot and build/process
//! identity — is the separate `diagnostics` bundle: different shape of data
//! (aggregated counts and versions, not log text), so it stays its own
//! bundle rather than growing this one past "the log itself."

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod list;
pub mod stream;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <stream::Stream as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "logs",
        label: "Server logs",
        operations: OPERATIONS,
    }
}
