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

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod automation_token {
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
}

pub mod federate {
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

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod list {
        use super::prelude::*;

        /// List the registered workload-identity federation mappings.
        ///
        /// Operator-only, same reasoning as [`create`](super::create):
        /// `user_grant_allows` refuses a plain `User` grant on every
        /// `/auth/federations` route.
        #[operation(
    id = "auth.federations.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "federation ls",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = Vec<FederationView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove a workload-identity federation mapping.
        ///
        /// Operator-only, same reasoning as [`create`](super::create).
        #[operation(
    id = "auth.federations.remove",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "federation rm",
)]
        pub struct Remove;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The mapping id (from `federation ls`).
            #[operand(positional)]
            pub id: String,
        }

        pub type Output = RemoveFederationResult;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
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
        ///
        /// Configuring how the whole fleet signs in is operator-only — `user_grant_allows`
        /// refuses a plain `User` grant on `/auth/github/config`.
        #[operation(
    id = "auth.github_config.get",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth github-config get",
)]
        pub struct Get;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = GithubConfigView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set {
        use super::prelude::*;

        /// Set the GitHub sign-in OAuth client id (and, optionally, its secret).
        ///
        /// Operator-only, same reasoning as [`get`](super::get).
        #[operation(
    id = "auth.github_config.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-config set",
)]
        pub struct Set;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            #[operand(positional)]
            pub client_id: String,
            /// Write-only: omit to leave the stored secret untouched, or pass an
            /// empty string to clear it.
            pub client_secret: Option<String>,
        }

        pub type Output = GithubConfigView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
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
        #[operation(
    id = "auth.github_token.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth github-token get",
)]
        pub struct Get;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = GithubTokenStatusView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove the caller's personal GitHub token.
        #[operation(
    id = "auth.github_token.remove",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-token rm",
)]
        pub struct Remove;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = GithubTokenStatusView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set {
        use super::prelude::*;

        /// Set the caller's personal GitHub token. Loom selects it for ordinary
        /// interactive sessions this user launches; restricted sessions never use
        /// it.
        #[operation(
    id = "auth.github_token.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth github-token set",
)]
        pub struct Set;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The token value. On the command line this names a file, or `-`/omitted
            /// to read stdin, so the secret need not sit in shell history.
            #[operand(positional, from_file)]
            pub token: String,
        }

        pub type Output = GithubTokenStatusView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod login {
    use super::prelude::*;

    /// Exchange a username and password for a signed-in session.
    ///
    /// `actor = Anonymous`: this is one of exactly two operations that create a
    /// credential, running before any credential exists. This is asserted by
    /// `anonymous_operations_are_pinned`.
    ///
    /// The CLI tool implements `loom login` separately by verifying a personal
    /// token against [`auth.me`](super::me) and [`auth.tokens.list`](super::tokens::list),
    /// then storing it locally. That's a hand-written Tier C client feature, so
    /// this operation has no `cli` projection.
    #[operation(
    id = "auth.login",
    actor = Anonymous,
    io = Session,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Login;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        pub username: String,
        pub password: String,
    }

    pub type Output = MeView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod logout {
    use super::prelude::*;

    /// End the caller's signed-in session.
    #[operation(
    id = "auth.logout",
    actor = User,
    io = Session,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Logout;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    /// The caller's identity after logout (`authenticated: false`).
    pub type Output = MeView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod me {
    use super::prelude::*;

    /// Who the caller is, and which sign-in methods the server offers.
    #[operation(
    id = "auth.me",
    actor = Anonymous,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth whoami",
)]
    pub struct Me;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = MeView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod set_password {
    use super::prelude::*;

    /// Set or change the caller's own password.
    #[operation(
    id = "auth.set_password",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth password",
)]
    pub struct SetPassword;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The new password (minimum 8 characters).
        pub new_password: String,
    }

    pub type Output = UserView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod tokens {
    //! The caller's own personal API tokens.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Mint a new personal API token. The plaintext is returned once — the
        /// server keeps only a hash.
        #[operation(
    id = "auth.tokens.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "token add",
)]
        pub struct Create;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// A label to recognise the token by (e.g. `github-actions`).
            #[operand(positional)]
            pub name: String,
            /// Optional lifetime in days; omitted or non-positive never expires.
            pub expires_in_days: Option<i64>,
        }

        pub type Output = CreatedTokenView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod list {
        use super::prelude::*;

        /// List the caller's own personal API tokens (metadata only — never the
        /// secret, see [`create`](super::create)).
        #[operation(
    id = "auth.tokens.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "token ls",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = Vec<TokenView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod revoke {
        use super::prelude::*;

        /// Revoke one of the caller's own personal API tokens.
        #[operation(
    id = "auth.tokens.revoke",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "token rm",
)]
        pub struct Revoke;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The token id (from `token ls`).
            #[operand(positional)]
            pub id: String,
        }

        pub type Output = RevokeTokenResult;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod users {
    //! The approved-operator allowlist: who may sign in, and with what role.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Approve a new operator, same reasoning as [`list`](super::list).
        #[operation(
    id = "auth.users.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth users add",
)]
        pub struct Create;

        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
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

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod list {
        use super::prelude::*;

        /// List the approved operators.
        #[operation(
    id = "auth.users.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth users ls",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = Vec<UserView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod remove {
        use super::prelude::*;

        /// Remove an approved operator. A caller may not remove themself.
        #[operation(
    id = "auth.users.remove",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "auth users rm",
)]
        pub struct Remove;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            #[operand(positional)]
            pub username: String,
        }

        pub type Output = RemoveUserResult;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set_role {
        use super::prelude::*;

        /// Change an operator's role. Existing cookies and personal tokens observe
        /// the change on their next request.
        #[operation(
    id = "auth.users.set_role",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "auth users role",
)]
        pub struct SetRole;

        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
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

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <me::Me as Operation>::SPEC,
    <login::Login as Operation>::SPEC,
    <logout::Logout as Operation>::SPEC,
    <federate::Federate as Operation>::SPEC,
    <automation_token::AutomationToken as Operation>::SPEC,
    <set_password::SetPassword as Operation>::SPEC,
    <tokens::list::List as Operation>::SPEC,
    <tokens::create::Create as Operation>::SPEC,
    <tokens::revoke::Revoke as Operation>::SPEC,
    <federations::list::List as Operation>::SPEC,
    <federations::create::Create as Operation>::SPEC,
    <federations::remove::Remove as Operation>::SPEC,
    <users::list::List as Operation>::SPEC,
    <users::create::Create as Operation>::SPEC,
    <users::set_role::SetRole as Operation>::SPEC,
    <users::remove::Remove as Operation>::SPEC,
    <github_token::get::Get as Operation>::SPEC,
    <github_token::set::Set as Operation>::SPEC,
    <github_token::remove::Remove as Operation>::SPEC,
    <github_config::get::Get as Operation>::SPEC,
    <github_config::set::Set as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "auth",
        label: "Authentication and access",
        operations: OPERATIONS,
    }
}
