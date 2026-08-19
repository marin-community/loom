use super::prelude::*;

/// Invoke one fixed-target GitHub operation granted by restricted session
/// policy.
#[operation(
    id = "permissions.github.restricted.invoke",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
pub struct Invoke;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The fixed restricted-GitHub tool to invoke, e.g. `issue_comment`.
    pub tool: String,
    /// Tool-specific arguments (`number`, optional `body`/`title`).
    #[operand(json)]
    pub arguments: serde_json::Value,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = RestrictedGithubToolView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
