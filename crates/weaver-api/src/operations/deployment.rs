//! Declarative reconciliation of runtime settings, launch profiles, and
//! workload federation mappings against one deployment stack.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod reconcile {
    use std::collections::BTreeMap;

    use super::prelude::*;

    /// Reconcile the runtime resources declared by a deployment stack: settings,
    /// launch profiles, and federation mappings.
    ///
    /// The manifest carries references and policy, never secret values.
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
}

static OPERATIONS: &[&OperationSpec] = &[<reconcile::Reconcile as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "deployment",
        label: "Deployment reconciliation",
        operations: OPERATIONS,
    }
}
