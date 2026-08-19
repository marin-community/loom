use super::prelude::*;

/// Set the GitHub sign-in OAuth client id (and, optionally, its secret).
///
/// Operator-only, same reasoning as [`get`](super::get).
#[operation(
    id = "auth.github_config.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-config set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    #[operand(positional)]
    pub client_id: String,
    /// Write-only: omit to leave the stored secret untouched, or pass an
    /// empty string to clear it.
    pub client_secret: Option<String>,
}

pub type Output = GithubConfigView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
