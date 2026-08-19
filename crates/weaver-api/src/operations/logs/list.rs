use super::prelude::*;

/// A snapshot of the most recent server log lines, oldest first. The UI loads
/// this once, then follows `logs.stream` for new lines.
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

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
