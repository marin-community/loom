use std::collections::BTreeMap;

use super::prelude::*;

/// Reconcile the runtime resources declared by a deployment stack: registered
/// settings, named launch profiles and their write-only environment, and
/// workload federation mappings. This is the API-first boundary Pulumi's
/// startup generation calls through the local Loom CLI; the manifest carries
/// references and policy, never secret values.
#[operation(
    id = "deployment.reconcile",
    actor = Admin,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
    cli = "deployment reconcile",
)]
pub struct Reconcile;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Organization defaults for registered runtime settings. Live database
    /// values remain a higher-precedence override.
    #[operand(json, default = BTreeMap::new())]
    pub settings: BTreeMap<String, DeploymentSettingValue>,
    /// Named profiles this stack declares, each with its write-only
    /// environment.
    #[operand(json, default = Vec::new())]
    pub profiles: Vec<DeploymentProfileReq>,
    /// Trusted GitHub Actions OIDC workflow mappings this stack declares.
    #[operand(json, default = Vec::new())]
    pub federations: Vec<FederationReq>,
    /// Remove previously deployment-managed resources omitted from this
    /// request.
    #[operand(default = false)]
    pub prune: bool,
}

pub type Output = DeploymentView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
