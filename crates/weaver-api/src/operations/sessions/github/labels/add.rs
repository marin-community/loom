use super::prelude::*;

/// Add labels to the pull request currently associated with a session.
/// Watch programs use this Loom-owned API instead of receiving a GitHub
/// credential and invoking `gh` themselves.
#[operation(
    id = "sessions.github.labels.add",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
pub struct Add;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// 1 to 10 label names to add to the pull request.
    #[serde(default)]
    pub labels: Vec<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

/// Result of `sessions.github.labels.add`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AddLabelsResult {
    pub number: i64,
    pub labels: Vec<String>,
}

pub type Output = AddLabelsResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
