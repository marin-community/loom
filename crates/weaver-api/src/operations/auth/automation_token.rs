use super::prelude::*;

/// Mint a short-lived automation-only token for a given subject.
///
/// Operator-only: `user_grant_allows` in `crates/loom/src/web/auth.rs`
/// refuses a plain `User` grant on `/auth/automation-token`, and the current
/// handler additionally checks `principal.is_admin()` by hand. Minting a
/// credential for some other automated subject is fleet administration, not
/// a self-service action — `actor = Admin`.
#[operation(
    id = "auth.automation_token",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "token mint",
)]
pub struct AutomationToken;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Stable identity recorded on runs launched with this token.
    #[operand(positional)]
    pub subject: String,
    /// Profiles the token may launch runs under.
    #[operand(long = "profile")]
    pub profiles: Vec<String>,
    /// Lifetime in seconds.
    #[operand(default = 600)]
    pub ttl_secs: i64,
}

pub type Output = AutomationTokenView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
