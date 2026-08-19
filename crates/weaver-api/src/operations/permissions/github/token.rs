use super::prelude::*;

/// Mint a refreshable repository-scoped GitHub App credential for this
/// session.
#[operation(
    id = "permissions.github.token",
    actor = SessionOnly,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
    cli = "github-token",
)]
pub struct Token;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = GithubTokenView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
