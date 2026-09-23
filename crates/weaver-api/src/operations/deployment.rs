//! Declarative reconciliation of runtime settings, launch profiles, and
//! workload federation mappings against one deployment stack.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod reconcile {
    use std::collections::BTreeMap;

    use super::prelude::*;

    /// Reconcile the runtime resources declared by a deployment stack: settings,
    /// remote MCP servers, launch profiles, and federation mappings.
    ///
    /// The manifest carries references and policy, never secret values.
    #[operation(id = "deployment.reconcile", actor = Admin, scope = Global, risk = ExternalWrite,
                cli = "deployment reconcile")]
    pub struct Input {
        /// Organization defaults for registered runtime settings. Live database
        /// values remain a higher-precedence override.
        #[operand(json, default = BTreeMap::new())]
        pub settings: BTreeMap<String, DeploymentSettingValue>,
        /// Remote Streamable HTTP MCP servers available to profile groups.
        #[operand(json, default = Vec::new())]
        pub remote_mcps: Vec<RemoteMcpReq>,
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

pub mod tokens {
    pub(super) use super::prelude;

    pub mod create {
        use super::prelude::*;

        /// Mint a credential that can only reconcile deployment configuration.
        #[operation(id = "deployment.tokens.create", actor = Admin, scope = Global, risk = Write,
                    cli = "deployment token add")]
        pub struct Input {
            #[operand(positional)]
            pub name: String,
            pub expires_in_days: Option<i64>,
        }

        pub type Output = CreatedTokenView;
    }

    pub mod list {
        use super::prelude::*;

        /// List deployment credentials without revealing their secrets.
        #[operation(id = "deployment.tokens.list", actor = Admin, scope = Global, risk = Read,
                    cli = "deployment token ls")]
        pub struct Input {}

        pub type Output = Vec<TokenView>;
    }

    pub mod revoke {
        use super::prelude::*;

        /// Revoke a deployment credential by id.
        #[operation(id = "deployment.tokens.revoke", actor = Admin, scope = Global, risk = Write,
                    cli = "deployment token rm")]
        pub struct Input {
            #[operand(positional)]
            pub id: String,
        }

        pub type Output = RevokeTokenResult;
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    reconcile::SPEC,
    tokens::create::SPEC,
    tokens::list::SPEC,
    tokens::revoke::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "deployment",
        label: "Deployment reconciliation",
        operations: OPERATIONS,
    }
}
