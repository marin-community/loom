use super::prelude::*;

/// List recent durable events on a branch (newest activity first, capped).
///
/// `GET /branches/{id}/events` looks like it should be a live tail, but its
/// handler (`branch_events` in `crates/loom/src/web/sessions.rs`, also
/// aliased at `GET /sessions/{id}/log`) returns a plain bounded
/// `Vec<Event>` — the last 200 rows — not an SSE stream. The live feed is
/// `sessions.events.stream` (`io = Stream`), keyed by session rather than
/// branch. So this stays `io = Json`, matching the already-registered
/// `sessions.events.list`, which wraps the same handler keyed by session.
#[operation(
    id = "branches.events.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches events list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = Vec<weaver_core::events::Event>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
