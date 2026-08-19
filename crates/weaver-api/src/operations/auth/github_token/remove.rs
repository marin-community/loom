use super::prelude::*;

/// Remove the caller's personal GitHub token.
#[operation(
    id = "auth.github_token.remove",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-token rm",
)]
pub struct Remove;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = GithubTokenStatusView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
