use super::prelude::*;

/// Replace the provider behind an idle ACP session while preserving Loom's
/// stable session/branch/worktree identity and canonical journal.
#[operation(
    id = "sessions.handoff",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions handoff",
)]
pub struct Handoff;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Legacy flattened runtime selector. Canonical clients use `selection`.
    #[operand(default = String::new())]
    pub agent: String,
    /// Blank/absent uses the target runtime's default.
    pub model: Option<String>,
    /// Blank/absent uses the target runtime's default.
    pub effort: Option<String>,
    /// ACP permission posture. Blank/absent uses the configured `agent.mode`.
    pub mode: Option<String>,
    /// The resolved profile and per-launch overrides, previewed beforehand.
    #[operand(skip_cli)]
    pub selection: Option<LaunchSelection>,
    /// Optimistic-concurrency guard against the previewed profile.
    #[operand(skip_cli)]
    pub expected_profile_revision: Option<i64>,
    /// Optimistic-concurrency guard against the previewed resolver snapshot.
    #[operand(skip_cli)]
    pub expected_resolver_revision: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
