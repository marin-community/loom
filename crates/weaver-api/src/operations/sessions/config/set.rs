use super::prelude::*;

/// Change one agent-owned session configuration selector. Waits for the
/// adapter's response and returns its full refreshed option list (also
/// broadcast to chat clients as a `metadata` event).
#[operation(
    id = "sessions.config.set",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which configuration selector to change.
    #[operand(positional)]
    pub config_id: String,
    /// The new value for this option.
    #[operand(json)]
    pub value: serde_json::Value,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.config.set`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigOptionResult {
    pub config_id: String,
    pub value: serde_json::Value,
    pub metadata: AcpMetadataView,
}

pub type Output = ConfigOptionResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
