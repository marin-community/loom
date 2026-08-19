use super::prelude::*;

/// Whether the caller has a personal GitHub token on file, and when it last
/// changed.
#[operation(
    id = "auth.github_token.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth github-token get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = GithubTokenStatusView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
