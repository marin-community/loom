use super::prelude::*;

/// Resolve a launch selection to its exact non-secret template snapshot —
/// agent, model, effort, protocol, mode, capacity, and provenance — without
/// launching a session. `loom sessions launch` runs this as a canonical
/// preflight; not exposed as its own CLI verb since callers reach it through
/// that preview instead.
///
/// Read-only. A session authorized to delegate a child launch may preview
/// the template it would launch with.
#[operation(
    id = "sessions.launches.resolve",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Resolve;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile and per-launch overrides to resolve.
    #[operand(skip_cli)]
    pub selection: LaunchSelection,
}

pub type Output = ResolvedLaunchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
