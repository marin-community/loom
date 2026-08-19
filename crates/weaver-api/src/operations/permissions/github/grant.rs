//! Granting repository access without a prior request is a human decision,
//! expressed through `actor = User`.

use super::prelude::*;

/// Directly grant one GitHub repository to a live session, without a prior
/// request.
#[operation(
    id = "permissions.github.grant",
    actor = User,
    scope = Session,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions grant github-repository",
)]
pub struct Grant;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The `owner/repo` slug to grant write access to.
    #[operand(positional)]
    pub repository: String,
    /// The session receiving access.
    pub session: String,
}

pub type Output = SessionGithubAccessView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
