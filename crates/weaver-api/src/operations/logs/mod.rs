//! The server log tail.
//!
//! Human-only diagnostics (Settings → Diagnostics), the streaming counterpart
//! of the `logview` snapshot handlers. Redaction is applied per subscriber
//! from the caller's own credential, which is why this cannot be a shared
//! broadcast served without a principal.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod stream;

static OPERATIONS: &[&OperationSpec] = &[<stream::Stream as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "logs",
        label: "Server logs",
        operations: OPERATIONS,
    }
}
