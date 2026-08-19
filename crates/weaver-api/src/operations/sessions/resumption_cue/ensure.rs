use super::prelude::*;

/// Generate the session's resumption cue if it is missing or stale. `force`
/// regenerates it unconditionally; otherwise the configured inactivity
/// threshold applies, as on the on-return path.
#[operation(
    id = "sessions.resumption_cue.ensure",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Ensure;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Regenerate unconditionally instead of respecting the inactivity
    /// threshold.
    #[operand(default = false)]
    pub force: bool,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = ResumptionCueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
