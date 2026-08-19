use super::prelude::*;

/// Exchange a workload-identity OIDC token for a short-lived automation
/// token, per a mapping an admin registered with `auth.federations.create`.
///
/// The caller is a CI system (e.g. a GitHub Actions job presenting its
/// runner OIDC token), never a human and never an agent session — the
/// textbook `actor = Internal` case. No prior Loom credential exists yet;
/// the OIDC token itself is what's verified, the same shape as
/// [`auth.login`](super::login) bootstrapping a session from a password.
#[operation(
    id = "auth.federate",
    actor = Internal,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Federate;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The workload-identity OIDC token to exchange.
    pub token: String,
}

pub type Output = AutomationTokenView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
