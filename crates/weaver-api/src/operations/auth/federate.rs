use super::prelude::*;

/// Exchange a workload-identity OIDC token for a short-lived automation
/// token, per a mapping an admin registered with `auth.federations.create`.
///
/// The caller is a CI system (e.g. a GitHub Actions job presenting its
/// runner OIDC token), never a human and never an agent session — the
/// `actor = Anonymous` — which does NOT mean unauthenticated. The caller proves
/// itself with an external OIDC token carried in the request body; what it lacks
/// is a *Loom* credential, so there is no `Principal` for `authorize` to inspect
/// and the operation must vouch for itself. The OIDC token itself is what's
/// verified, similar to [`auth.login`](super::login) bootstrapping a session
/// from a password.
#[operation(
    id = "auth.federate",
    actor = Anonymous,
    io = Session,
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
