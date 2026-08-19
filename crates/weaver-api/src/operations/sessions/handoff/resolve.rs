use super::prelude::*;

/// Preview a handoff without applying it: resolve a selection to the exact
/// non-secret template snapshot [`super::Handoff`] would replace the current
/// runtime with, the same way `sessions.launches.resolve` previews a fresh
/// launch.
///
/// Same grant as `sessions.handoff` itself, even though this is `risk =
/// Read`: a session entitled to hand itself off gains no new surface by
/// previewing what that would produce, matching the reasoning documented on
/// `sessions.launches.resolve`.
#[operation(
    id = "sessions.handoff.resolve",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Resolve;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile and per-launch overrides to resolve.
    #[operand(skip_cli)]
    pub selection: LaunchSelection,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = ResolvedLaunchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
