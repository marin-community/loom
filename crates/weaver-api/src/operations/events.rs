//! The multiplexed event stream.
//!
//! One bundle, one operation: the single SSE connection a browser tab holds
//! open instead of spending one of its ~6 per-origin sockets per panel. It is
//! registered like everything else — the only thing `io = Stream` changes is
//! that the response is an event stream, so a custom handler serves it rather
//! than the generic JSON dispatcher.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod stream {
    use super::prelude::*;

    /// Subscribe to one or more event topics over a single SSE connection.
    ///
    /// Reaching this operation grants nothing on its own: every topic in
    /// `topics` is authorized separately against the single-topic stream it
    /// stands in for. The grant named here is the floor those topics share.
    #[operation(id = "events.stream", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/sessions/read@v1"], io = Stream)]
    pub struct Input {
        /// Comma-separated topic list: `layout`, `logs`, `session:<key>`,
        /// `chat:<key>`. Empty parks the connection on keep-alive.
        //
        // `serde(default)` because a stream's operands arrive in the query string,
        // which is extracted before the dispatcher's default-filling step runs.
        #[serde(default)]
        #[operand(default = String::new())]
        pub topics: String,
    }

    pub type Output = ();
}

static OPERATIONS: &[&OperationSpec] = &[stream::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "events",
        label: "Event stream",
        operations: OPERATIONS,
    }
}
