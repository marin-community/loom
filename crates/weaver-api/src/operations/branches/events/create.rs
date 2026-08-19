use super::prelude::*;

/// Append a raw event row to a branch's log — the escape hatch for an event
/// kind with no dedicated mutating route of its own.
///
/// The branch-scoped twin of `sessions.events.create`, split off the same
/// `GET/POST /branches/{id}/events` route as `branches.events.list`.
#[operation(
    id = "branches.events.create",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/branches/write@v1"],
    cli = "branches events create",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The event kind, e.g. an agent hook name.
    pub kind: String,
    /// Arbitrary event payload.
    #[operand(json, default = serde_json::Value::Null)]
    pub data: serde_json::Value,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = weaver_core::events::Event;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
