//! The server log text: a snapshot (`list`) and a live tail (`stream`).
//!
//! Human-only diagnostics (Settings → Diagnostics). Redaction is applied per
//! subscriber from the caller's own credential, which is why the stream
//! cannot be a shared broadcast served without a principal. The structured
//! fleet health snapshot and build/process identity are in the separate
//! `diagnostics` bundle.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod list {
    use super::prelude::*;

    /// A snapshot of the most recent server log lines, oldest first.
    ///
    /// `actor = User`: the snapshot counterpart of `logs.stream` — same policy,
    /// same reasoning (see that operation's doc comment).
    #[operation(
    id = "logs.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Most-recent lines to return. Clamped to the buffer size; defaults to
        /// 500.
        pub limit: Option<i64>,
    }

    /// One captured log line, as the UI renders it.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    pub struct LogLineView {
        /// Monotonic sequence number, so the UI can dedupe the snapshot against
        /// the live stream (and detect drops) without comparing timestamps.
        pub seq: u64,
        /// RFC3339 UTC timestamp.
        pub ts: String,
        /// `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`.
        pub level: String,
        /// The event's target (module path, e.g. `loom::web::repos`).
        pub target: String,
        /// The rendered message plus any structured fields.
        pub message: String,
    }

    pub type Output = Vec<LogLineView>;
}

pub mod stream {
    use super::prelude::*;

    /// Tail the server log as it is written.
    ///
    /// `actor = User`: human-only self-service debugging; no session grant can
    /// reach the log routes.
    #[operation(
    id = "logs.stream",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    io = Stream,
)]
    pub struct Stream;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = ();
}

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
