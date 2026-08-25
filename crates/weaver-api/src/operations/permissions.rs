//! Access discovery and human-approved external credential expansion.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod effective {
    //! This session's effective operations, GitHub scope, and pending requests.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Show this session's effective Loom operations and external repository
        /// scope.
        #[operation(id = "permissions.effective.get", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/permissions/read@v1"], cli = "permissions show")]
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
    #[operation(id = "permissions.explain", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/permissions/read@v1"], cli = "permissions explain")]
    pub struct Input {
        /// The operation id to explain, e.g. `issues.tags.set`.
        #[operand(positional)]
        #[schemars(length(min = 1))]
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
        #[operation(id = "permissions.github.grant", actor = User, scope = Session,
                    risk = ExternalWrite, cli = "permissions grant github-repository")]
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

        use serde::{Deserialize, Serialize};

        pub const BODY_MAX_BYTES: usize = 65_536;
        pub const TITLE_MAX_BYTES: usize = 256;

        /// One issue or pull request in the repository fixed to the session.
        #[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
        pub struct Target {
            /// The issue or pull-request number.
            #[schemars(range(min = 1))]
            pub number: i64,
        }

        /// A comment to post on one.
        #[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
        pub struct Comment {
            /// The issue or pull-request number.
            #[schemars(range(min = 1))]
            pub number: i64,
            /// The comment text.
            #[schemars(length(max = 65_536))]
            pub body: String,
        }

        /// A replacement body for one, optionally retitling it.
        #[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
        pub struct Edit {
            /// The issue or pull-request number.
            #[schemars(range(min = 1))]
            pub number: i64,
            /// The replacement body.
            #[schemars(length(max = 65_536))]
            pub body: String,
            /// A replacement title. Omit to leave the title alone.
            #[schemars(length(min = 1, max = 256))]
            pub title: Option<String>,
        }

        // The bounds above are schemars literals, which cannot name a constant.
        const _: () = assert!(BODY_MAX_BYTES == 65_536 && TITLE_MAX_BYTES == 256);

        /// One fixed tool `invoke` serves: the name a transport advertises, what
        /// it does, and the shape of the `arguments` it takes.
        ///
        /// `invoke` takes `arguments` as an opaque object because one operation
        /// serves all six, so the shapes are declared here rather than by
        /// whichever transport happens to expose them.
        pub struct Tool {
            pub name: &'static str,
            pub summary: &'static str,
            pub schema: fn() -> serde_json::Value,
        }

        fn schema_of<T: schemars::JsonSchema>() -> serde_json::Value {
            serde_json::to_value(schemars::schema_for!(T))
                .unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
        }

        pub const TOOLS: &[Tool] = &[
            Tool {
                name: "issue_view",
                summary: "Read one issue in the GitHub repository fixed to this session.",
                schema: schema_of::<Target>,
            },
            Tool {
                name: "issue_comment",
                summary: "Post a comment on one issue in the GitHub repository fixed to \
                          this session.",
                schema: schema_of::<Comment>,
            },
            Tool {
                name: "issue_edit",
                summary: "Replace an issue body and optionally its title in the GitHub \
                          repository fixed to this session.",
                schema: schema_of::<Edit>,
            },
            Tool {
                name: "pr_view",
                summary: "Read one pull request in the GitHub repository fixed to this session.",
                schema: schema_of::<Target>,
            },
            Tool {
                name: "pr_comment",
                summary: "Post a comment on one pull request in the GitHub repository fixed \
                          to this session.",
                schema: schema_of::<Comment>,
            },
            Tool {
                name: "pr_edit",
                summary: "Replace a pull-request body and optionally its title in the GitHub \
                          repository fixed to this session.",
                schema: schema_of::<Edit>,
            },
        ];
        pub mod invoke {
            use super::prelude::*;

            /// Invoke one fixed-target GitHub operation granted by restricted session
            /// policy.
            #[operation(id = "permissions.github.restricted.invoke", actor = SessionSelf,
                        scope = Session, risk = ExternalWrite, grants = ["loom/github/use@v1"])]
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
        #[operation(id = "permissions.github.revoke", actor = User, scope = Session,
                    risk = ExternalWrite, cli = "permissions revoke github-repository")]
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
        #[operation(id = "permissions.github.token", actor = SessionOnly, scope = Session,
                    risk = ExternalWrite, grants = ["loom/github/use@v1"], cli = "github-token")]
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
        #[operation(id = "permissions.requests.approve", actor = User, scope = Global,
                    risk = ExternalWrite, cli = "permissions approve")]
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
        #[operation(id = "permissions.requests.create", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/permissions/request@v1"],
                    cli = "permissions request github-repository")]
        pub struct Input {
            /// The `owner/repo` slug to request write access to.
            #[operand(positional)]
            #[schemars(regex(pattern = r"^[^/]+/[^/]+$"))]
            pub repository: String,
            /// Why the task needs this repository.
            #[schemars(length(min = 1, max = 4096))]
            pub reason: String,
            /// Currently only `write` is accepted.
            #[operand(default = "write")]
            #[schemars(extend("enum" = ["write"]))]
            pub mode: String,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = PermissionRequestView;
    }

    pub mod deny {
        use super::prelude::*;

        /// Deny a pending external-access request.
        #[operation(id = "permissions.requests.deny", actor = User, scope = Global, risk = Write,
                    cli = "permissions deny")]
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
        #[operation(id = "permissions.requests.list", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/permissions/read@v1"],
                    cli = "permissions requests")]
        pub struct Input {
            /// Restrict to `pending`, `approved`, or `denied`. Omit to list all.
            #[schemars(extend("enum" = ["pending", "approved", "denied"]))]
            pub state: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = Vec<PermissionRequestView>;
    }
}

/// Maximum byte length of a permission-request reason.
pub const MAX_REASON_LEN: usize = 4_096;
// `requests::create::Input` spells this bound as a schemars literal, which
// cannot reference a constant.
const _: () = assert!(MAX_REASON_LEN == 4_096);

static OPERATIONS: &[&OperationSpec] = &[
    effective::get::SPEC,
    explain::SPEC,
    requests::list::SPEC,
    requests::create::SPEC,
    requests::approve::SPEC,
    requests::deny::SPEC,
    github::grant::SPEC,
    github::revoke::SPEC,
    github::token::SPEC,
    github::restricted::invoke::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "permissions",
        label: "Access and approvals",
        operations: OPERATIONS,
    }
}
