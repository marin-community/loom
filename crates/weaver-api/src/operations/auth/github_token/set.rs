use super::prelude::*;

/// Set the caller's personal GitHub token. Loom selects it for ordinary
/// interactive sessions this user launches; restricted sessions never use
/// it.
#[operation(
    id = "auth.github_token.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-token set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The token value. On the command line this names a file, or `-`/omitted
    /// to read stdin, so the secret need not sit in shell history.
    #[operand(positional, from_file)]
    pub token: String,
}

pub type Output = GithubTokenStatusView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
