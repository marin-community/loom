use super::prelude::*;

/// Shared upload limits for launch-time and live-session Scratch attachments.
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
