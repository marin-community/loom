use super::prelude::*;

/// Register (or idempotently reconcile) a workload-identity federation
/// mapping — the trust relationship `auth.federate` exchanges an OIDC token
/// against.
///
/// Fleet configuration, not a self-service action: `user_grant_allows`
/// refuses a plain `User` grant on every mutating `/auth/federations` route,
/// so this is `actor = Admin`.
#[operation(
    id = "auth.federations.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "federation add",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Stable operator-owned identity used for idempotent reconciliation.
    /// Omitted legacy calls derive one from the identity fields below.
    pub name: Option<String>,
    #[operand(default = String::from("github"))]
    pub provider: String,
    #[operand(default = String::from("https://token.actions.githubusercontent.com"))]
    pub issuer: String,
    pub audience: String,
    /// Exact numeric OIDC subject for Google workload identities.
    pub subject: Option<String>,
    /// Exact verified Google service-account email.
    pub service_account: Option<String>,
    /// Stable, bounded audit label copied into Loom automation credentials.
    #[operand(default = String::from("github-actions"))]
    pub service_tag: String,
    pub repository_id: Option<String>,
    pub workflow_ref: Option<String>,
    #[operand(long = "event")]
    pub event_name: Option<String>,
    #[operand(long = "ref")]
    pub ref_pattern: Option<String>,
    /// Profiles a token minted through this mapping may launch runs under.
    #[operand(long = "profile")]
    pub profiles: Vec<String>,
}

pub type Output = FederationView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
