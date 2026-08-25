//! The server log text: a snapshot (`list`) and a live tail (`stream`).
//!
//! Human-only diagnostics (Settings → Diagnostics). Redaction is applied per
//! subscriber from the caller's own credential, which is why the stream
//! cannot be a shared broadcast served without a principal. The structured
//! fleet health snapshot and build/process identity are in the separate
//! `diagnostics` bundle.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod list {
    use super::prelude::*;

    /// A snapshot of the most recent server log lines, oldest first.
    #[operation(id = "logs.list", actor = User, scope = Global, risk = Read)]
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
    #[operation(id = "logs.stream", actor = User, scope = Global, risk = Read, io = Stream)]
    pub struct Input {}

    pub type Output = ();
}

static OPERATIONS: &[&OperationSpec] = &[list::SPEC, stream::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "logs",
        label: "Server logs",
        operations: OPERATIONS,
    }
}
