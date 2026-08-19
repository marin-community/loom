use super::prelude::*;

/// Read the GitHub sign-in / App setup (secret withheld).
///
/// Previously excluded from the registry as "administrative". Configuring
/// how the whole fleet signs in is operator-only — `user_grant_allows`
/// refuses a plain `User` grant on `/auth/github/config`.
#[operation(
    id = "auth.github_config.get",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth github-config get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = GithubConfigView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
