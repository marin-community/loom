//! The multiplexed event stream.
//!
//! One bundle, one operation: the single SSE connection a browser tab holds
//! open instead of spending one of its ~6 per-origin sockets per panel. It is
//! registered like everything else — the only thing `io = Stream` changes is
//! that the response is an event stream, so a custom handler serves it rather
//! than the generic JSON dispatcher.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod stream;

static OPERATIONS: &[&OperationSpec] = &[<stream::Stream as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "events",
        label: "Event stream",
        operations: OPERATIONS,
    }
}
