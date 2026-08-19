use super::prelude::*;

/// List the repository access a session has been granted.
///
/// `actor = User` and no grant: this is a human read *about* an agent, and the
/// route it replaces called `require_human` for that reason. A session that
/// wants to know what it may reach asks GitHub, or fails and reads the error.
#[operation(
    id = "sessions.github.access.list",
    actor = User,
    scope = Session,
    risk = Read,
    grants = [],
    cli = "sessions github access",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id.
    #[operand(positional)]
    pub session: String,
}

pub type Output = Vec<SessionGithubAccessView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
