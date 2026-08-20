//! Access discovery and human-approved external credential expansion.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod effective {
    //! This session's effective operations, GitHub scope, and pending requests.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Show this session's effective Loom operations and external repository
        /// scope.
        #[operation(
    id = "permissions.effective.get",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions show",
    mcp = "loom_permission::show",
)]
        pub struct Get;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = EffectivePermissionsView;
    }
}

pub mod explain {
    use super::prelude::*;

    /// Explain one registered operation's actor, risk, and projections.
    #[operation(
    id = "permissions.explain",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions explain",
    mcp = "loom_permission::explain",
)]
    pub struct Explain;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The operation id to explain, e.g. `issues.tags.set`.
        #[operand(positional)]
        pub operation: String,
    }

    pub type Output = crate::operations::OperationView;
}

pub mod github {
    //! Session-scoped GitHub App credentials and human-authorized overrides.
    pub(super) use super::prelude;
    pub mod grant {
        //! Granting repository access without a prior request is a human decision,
        //! expressed through `actor = User`.

        use super::prelude::*;

        /// Directly grant one GitHub repository to a live session, without a prior
        /// request.
        #[operation(
    id = "permissions.github.grant",
    actor = User,
    scope = Session,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions grant github-repository",
)]
        pub struct Grant;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The `owner/repo` slug to grant write access to.
            #[operand(positional)]
            pub repository: String,
            /// The session receiving access.
            pub session: String,
        }

        pub type Output = SessionGithubAccessView;
    }

    pub mod restricted {
        //! The fixed-target GitHub surface exposed to policy-restricted sessions.
        pub(super) use super::prelude;
        pub mod invoke {
            use super::prelude::*;

            /// Invoke one fixed-target GitHub operation granted by restricted session
            /// policy.
            #[operation(
    id = "permissions.github.restricted.invoke",
    actor = SessionSelf,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
)]
            pub struct Invoke;

            #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
            pub struct Input {
                /// The fixed restricted-GitHub tool to invoke, e.g. `issue_comment`.
                pub tool: String,
                /// Tool-specific arguments (`number`, optional `body`/`title`).
                #[operand(json)]
                pub arguments: serde_json::Value,
                #[operand(context)]
                pub session: String,
            }

            pub type Output = RestrictedGithubToolView;
        }
    }

    pub mod revoke {
        //! Revoke explicit GitHub repository access from a live session.
        //! This is expressed through `actor = User`.

        use super::prelude::*;

        /// Revoke one explicit GitHub repository override from a live session.
        #[operation(
    id = "permissions.github.revoke",
    actor = User,
    scope = Session,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions revoke github-repository",
)]
        pub struct Revoke;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The `owner/repo` slug to revoke write access from.
            #[operand(positional)]
            pub repository: String,
            /// The session losing access.
            pub session: String,
        }

        pub type Output = SessionGithubAccessView;
    }

    pub mod token {
        use super::prelude::*;

        /// Mint a refreshable repository-scoped GitHub App credential for this
        /// session.
        #[operation(
    id = "permissions.github.token",
    actor = SessionOnly,
    scope = Session,
    risk = ExternalWrite,
    grants = ["loom/github/use@v1"],
    cli = "github-token",
)]
        pub struct Token;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = GithubTokenView;
    }
}

pub mod requests {
    //! Durable, human-decided external-access requests.
    pub(super) use super::prelude;
    pub mod approve {
        //! A human decides a pending request. This operation uses `actor = User`,
        //! which means it is not available to agents.

        use super::prelude::*;

        /// Approve and apply a pending external-access request.
        #[operation(
    id = "permissions.requests.approve",
    actor = User,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions approve",
)]
        pub struct Approve;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The pending permission request id.
            #[operand(positional)]
            pub request: String,
            /// Optional audit reason recorded with the decision.
            #[operand(default = String::new())]
            pub reason: String,
        }

        pub type Output = PermissionRequestView;
    }

    pub mod create {
        use super::prelude::*;

        /// Request a human-approved GitHub write-access expansion for this session.
        #[operation(
    id = "permissions.requests.create",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/permissions/request@v1"],
    cli = "permissions request github-repository",
    mcp = "loom_permission::request",
)]
        pub struct Create;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The `owner/repo` slug to request write access to.
            #[operand(positional)]
            pub repository: String,
            /// Why the task needs this repository.
            pub reason: String,
            /// Currently only `write` is accepted.
            #[operand(default = "write")]
            pub mode: String,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = PermissionRequestView;
    }

    pub mod deny {
        use super::prelude::*;

        /// Deny a pending external-access request.
        #[operation(
    id = "permissions.requests.deny",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "permissions deny",
)]
        pub struct Deny;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The pending permission request id.
            #[operand(positional)]
            pub request: String,
            /// Optional audit reason recorded with the decision.
            #[operand(default = String::new())]
            pub reason: String,
        }

        pub type Output = PermissionRequestView;
    }

    pub mod list {
        use super::prelude::*;

        /// List durable external-access requests for this session.
        #[operation(
    id = "permissions.requests.list",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions requests",
    mcp = "loom_permission::requests",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// Restrict to `pending`, `approved`, or `denied`. Omit to list all.
            pub state: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = Vec<PermissionRequestView>;
    }
}

/// Maximum byte length of a permission-request reason.
pub const MAX_REASON_LEN: usize = 4_096;

static OPERATIONS: &[&OperationSpec] = &[
    <effective::get::Get as Operation>::SPEC,
    <explain::Explain as Operation>::SPEC,
    <requests::list::List as Operation>::SPEC,
    <requests::create::Create as Operation>::SPEC,
    <requests::approve::Approve as Operation>::SPEC,
    <requests::deny::Deny as Operation>::SPEC,
    <github::grant::Grant as Operation>::SPEC,
    <github::revoke::Revoke as Operation>::SPEC,
    <github::token::Token as Operation>::SPEC,
    <github::restricted::invoke::Invoke as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "permissions",
        label: "Access and approvals",
        operations: OPERATIONS,
    }
}
