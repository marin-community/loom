use super::prelude::*;

/// Page normalized session history records.
#[operation(
    id = "sessions.history.list",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    mcp = "loom_session::history",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Page backward from this cursor (exclusive). Omit for the newest tail.
    pub before: Option<String>,
    /// Maximum records to return (1-200).
    pub limit: Option<i64>,
    /// Restrict to these record kinds: `message`, `reasoning`, `tool_call`,
    /// `tool_result`, `context`, `event`, or `image`.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = HistoryPageView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
