use super::prelude::*;

/// Shared upload limits for launch-time and live-session Scratch attachments.
///
/// `actor = User`: the legacy route this replaces is reachable only by a human
/// or admin credential today — `grant_allows` (`crates/loom/src/web/auth.rs`)
/// allows any `GET` for `Grant::User` but has no case admitting `Grant::Session`
/// to this path, unlike the `/sessions/{id}/...` family it sits beside.
#[operation(
    id = "sessions.scratch.limits",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "sessions scratch limits",
)]
pub struct Limits;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = ScratchLimitsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
