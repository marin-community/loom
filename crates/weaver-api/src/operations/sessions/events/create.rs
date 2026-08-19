use super::prelude::*;

/// Record a trusted agent lifecycle event.
#[operation(
    id = "sessions.events.create",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "hook",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The event kind, e.g. an agent hook name.
    #[operand(long = "event")]
    pub kind: String,
    /// Arbitrary event payload.
    #[operand(json, default = serde_json::Value::Null)]
    pub data: serde_json::Value,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = weaver_core::events::Event;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
