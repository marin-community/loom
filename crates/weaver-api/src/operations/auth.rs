//! Identity, credentials, and fleet-wide access administration.
//!
//! Operations use `actor = User` for identity self-management (sign-in,
//! token creation, password changes), `actor = Admin` for fleet administration
//! (user approval, GitHub sign-in configuration, federation mappings, automation
//! tokens), and `actor = Anonymous` for `auth.federate` (CI systems exchanging
//! workload-identity tokens).
//!
//! Browser OAuth endpoints (`GET /auth/github/login`, `GET /auth/github/callback`)
//! are not registered here: they return redirects, not JSON, and are never called
//! programmatically.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod automation_token {
    use super::prelude::*;

    /// Mint a short-lived automation-only token for a given subject.
    #[operation(id = "auth.automation_token", actor = Admin, scope = Global, risk = Write,
                cli = "token mint")]
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
}

pub mod federate {
    use super::prelude::*;

    /// Exchange a workload-identity OIDC token for a short-lived automation
    /// token, per a mapping an admin registered with `auth.federations.create`.
    ///
    /// The caller is a CI system presenting an external OIDC token — a GitHub
    /// Actions job with its runner token, say. `actor = Anonymous` here means
    /// it holds no *Loom* credential, not that it is unauthenticated: the OIDC
    /// token is verified in the request body, so there is no `Principal` for
    /// `authorize` to inspect and the operation vouches for itself.
    #[operation(id = "auth.federate", actor = Anonymous, io = Session, scope = Global, risk = Write)]
    pub struct Input {
        /// The workload-identity OIDC token to exchange.
        pub token: String,
    }

    pub type Output = AutomationTokenView;
}

pub mod federations {
    //! Workload-identity (OIDC) trust mappings that `auth.federate` exchanges
    //! against.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Register (or idempotently reconcile) a workload-identity federation
        /// mapping — the trust relationship `auth.federate` exchanges an OIDC token
        /// against.
        #[operation(id = "auth.federations.create", actor = Admin, scope = Global, risk = Write,
                    cli = "federation add")]
        pub struct Input {
            /// Stable operator-owned identity used for idempotent reconciliation.
            /// When omitted, one is derived from the identity fields below.
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
    }

    pub mod list {
        use super::prelude::*;

        /// List the registered workload-identity federation mappings.
        #[operation(id = "auth.federations.list", actor = Admin, scope = Global, risk = Read,
                    cli = "federation ls")]
        pub struct Input {}

        pub type Output = Vec<FederationView>;
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove a workload-identity federation mapping.
        #[operation(id = "auth.federations.remove", actor = Admin, scope = Global, risk = Write,
                    cli = "federation rm")]
        pub struct Input {
            /// The mapping id (from `federation ls`).
            #[operand(positional)]
            pub id: String,
        }

        pub type Output = RemoveFederationResult;
    }
}

pub mod github_config {
    //! The GitHub App / OAuth sign-in setup. One App backs loom: its OAuth
    //! client powers "Sign in with GitHub"; the same App's id and private key
    //! power the `@loom` trigger.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Read the GitHub sign-in / App setup (secret withheld).
        #[operation(id = "auth.github_config.get", actor = Admin, scope = Global, risk = Read,
                    cli = "auth github-config get")]
        pub struct Input {}

        pub type Output = GithubConfigView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set the GitHub sign-in OAuth client id (and, optionally, its secret).
        #[operation(id = "auth.github_config.set", actor = Admin, scope = Global, risk = Write,
                    cli = "auth github-config set")]
        pub struct Input {
            #[operand(positional)]
            pub client_id: String,
            /// Write-only: omit to leave the stored secret untouched, or pass an
            /// empty string to clear it.
            pub client_secret: Option<String>,
        }

        pub type Output = GithubConfigView;
    }
}

pub mod github_token {
    //! The caller's own GitHub personal-access token, injected into their
    //! ordinary interactive sessions. Write-only: no operation here ever returns
    //! the token value, only whether one is set.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Whether the caller has a personal GitHub token on file, and when it last
        /// changed.
        #[operation(id = "auth.github_token.get", actor = User, scope = Global, risk = Read,
                    cli = "auth github-token get")]
        pub struct Input {}

        pub type Output = GithubTokenStatusView;
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove the caller's personal GitHub token.
        #[operation(id = "auth.github_token.remove", actor = User, scope = Global, risk = Write,
                    cli = "auth github-token rm")]
        pub struct Input {}

        pub type Output = GithubTokenStatusView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set the caller's personal GitHub token. Loom selects it for ordinary
        /// interactive sessions this user launches; restricted sessions never use
        /// it.
        #[operation(id = "auth.github_token.set", actor = User, scope = Global, risk = Write,
                    cli = "auth github-token set")]
        pub struct Input {
            /// The token value. On the command line this names a file, or `-`/omitted
            /// to read stdin, so the secret need not sit in shell history.
            #[operand(positional, from_file)]
            pub token: String,
        }

        pub type Output = GithubTokenStatusView;
    }
}

pub mod login {
    use super::prelude::*;

    /// Exchange a username and password for a signed-in session.
    ///
    /// One of exactly two operations that create a credential, so it runs
    /// before any credential exists.
    #[operation(id = "auth.login", actor = Anonymous, io = Session, scope = Global, risk = Write)]
    pub struct Input {
        pub username: String,
        pub password: String,
    }

    pub type Output = MeView;
}

pub mod logout {
    use super::prelude::*;

    /// End the caller's signed-in session.
    #[operation(id = "auth.logout", actor = User, io = Session, scope = Global, risk = Write)]
    pub struct Input {}

    /// The caller's identity after logout (`authenticated: false`).
    pub type Output = MeView;
}

pub mod me {
    use super::prelude::*;

    /// Who the caller is, and which sign-in methods the server offers.
    #[operation(id = "auth.me", actor = Anonymous, scope = Global, risk = Read, cli = "auth whoami")]
    pub struct Input {}

    pub type Output = MeView;
}

pub mod set_password {
    use super::prelude::*;

    /// Set or change the caller's own password.
    #[operation(id = "auth.set_password", actor = User, scope = Global, risk = Write,
                cli = "auth password")]
    pub struct Input {
        /// The new password (minimum 8 characters).
        pub new_password: String,
    }

    pub type Output = UserView;
}

pub mod tokens {
    //! The caller's own personal API tokens.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Mint a new personal API token. The plaintext is returned once — the
        /// server keeps only a hash.
        #[operation(id = "auth.tokens.create", actor = User, scope = Global, risk = Write,
                    cli = "token add")]
        pub struct Input {
            /// A label to recognise the token by (e.g. `github-actions`).
            #[operand(positional)]
            pub name: String,
            /// Optional lifetime in days; omitted or non-positive never expires.
            pub expires_in_days: Option<i64>,
        }

        pub type Output = CreatedTokenView;
    }

    pub mod list {
        use super::prelude::*;

        /// List the caller's own personal API tokens (metadata only; secrets are
        /// never returned).
        #[operation(id = "auth.tokens.list", actor = User, scope = Global, risk = Read,
                    cli = "token ls")]
        pub struct Input {}

        pub type Output = Vec<TokenView>;
    }

    pub mod revoke {
        use super::prelude::*;

        /// Revoke one of the caller's own personal API tokens.
        #[operation(id = "auth.tokens.revoke", actor = User, scope = Global, risk = Write,
                    cli = "token rm")]
        pub struct Input {
            /// The token id (from `token ls`).
            #[operand(positional)]
            pub id: String,
        }

        pub type Output = RevokeTokenResult;
    }
}

pub mod users {
    //! The approved-operator allowlist: who may sign in, and with what role.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Add a new operator to the approved allowlist.
        #[operation(id = "auth.users.create", actor = Admin, scope = Global, risk = Write,
                    cli = "auth users add", default = custom)]
        pub struct Input {
            #[operand(positional)]
            pub username: String,
            /// The GitHub login allowed to sign in as this operator.
            pub github_login: Option<String>,
            /// A password, if this operator should also be able to sign in with one.
            /// At least one of `github_login` or `password` is required.
            pub password: Option<String>,
            /// `admin` or `user`.
            #[operand(json, default = UserRole::User)]
            pub role: UserRole,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    username: String::new(),
                    github_login: None,
                    password: None,
                    role: UserRole::User,
                }
            }
        }

        pub type Output = UserView;
    }

    pub mod list {
        use super::prelude::*;

        /// List the approved operators.
        #[operation(id = "auth.users.list", actor = Admin, scope = Global, risk = Read,
                    cli = "auth users ls")]
        pub struct Input {}

        pub type Output = Vec<UserView>;
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove an approved operator. A caller may not remove themself.
        #[operation(id = "auth.users.remove", actor = Admin, scope = Global, risk = Destructive,
                    cli = "auth users rm")]
        pub struct Input {
            #[operand(positional)]
            pub username: String,
        }

        pub type Output = RemoveUserResult;
    }

    pub mod set_role {
        use super::prelude::*;

        /// Change an operator's role. Existing cookies and personal tokens observe
        /// the change on their next request.
        #[operation(id = "auth.users.set_role", actor = Admin, scope = Global, risk = Write,
                    cli = "auth users role", default = custom)]
        pub struct Input {
            #[operand(positional)]
            pub username: String,
            /// `admin` or `user`.
            #[operand(json)]
            pub role: UserRole,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    username: String::new(),
                    role: UserRole::User,
                }
            }
        }

        pub type Output = UserView;
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    me::SPEC,
    login::SPEC,
    logout::SPEC,
    federate::SPEC,
    automation_token::SPEC,
    set_password::SPEC,
    tokens::list::SPEC,
    tokens::create::SPEC,
    tokens::revoke::SPEC,
    federations::list::SPEC,
    federations::create::SPEC,
    federations::remove::SPEC,
    users::list::SPEC,
    users::create::SPEC,
    users::set_role::SPEC,
    users::remove::SPEC,
    github_token::get::SPEC,
    github_token::set::SPEC,
    github_token::remove::SPEC,
    github_config::get::SPEC,
    github_config::set::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "auth",
        label: "Authentication and access",
        operations: OPERATIONS,
    }
}
