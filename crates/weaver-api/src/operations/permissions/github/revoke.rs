//! Revoke explicit GitHub repository access from a live session. Like `grant`,
//! this is expressed through `actor = User`.

use super::prelude::*;

/// Revoke one explicit GitHub repository override from a live session.
#[operation(
    id = "permissions.github.revoke",
    actor = User,
    scope = Session,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions revoke github-repository",
)]
pub struct Revoke;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The `owner/repo` slug to revoke write access from.
    #[operand(positional)]
    pub repository: String,
    /// The session losing access.
    pub session: String,
}

pub type Output = SessionGithubAccessView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
