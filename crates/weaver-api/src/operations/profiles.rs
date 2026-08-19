//! Named session-launch profiles and their write-only environment.
//!
//! A profile is a reusable, named launch template — the agent runtime,
//! model, MCP policy, and other launch-time policy a session inherits by
//! name. Secret environment values are write-only: every read-side view
//! carries metadata only, never a stored value.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod clone {
    use super::prelude::*;

    /// Clone one profile's reviewed policy into a new insert-only profile,
    /// optionally composing its write-only environment in the same transaction.
    /// Loom guards both the source profile's revision and the resolver
    /// fingerprint the caller reviewed; a drift in either returns a fresh
    /// preview instead of silently applying a stale composition.
    #[operation(
    id = "profiles.clone",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles clone",
)]
    pub struct Clone;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile being cloned.
        #[operand(positional)]
        pub source: String,
        /// The new profile's name.
        #[operand(positional)]
        pub name: String,
        /// Revision of `source` the caller reviewed; a 409 with a fresh preview
        /// means it has since changed.
        pub expected_profile_revision: i64,
        /// Resolver fingerprint from the composition the caller reviewed.
        pub expected_resolver_revision: String,
        /// Fields to layer over the source profile for this one resolution.
        #[operand(json, default = LaunchOverrides::default())]
        pub overrides: LaunchOverrides,
        /// Optional fully edited profile proposal. Omitted copies the source
        /// profile's policy verbatim; source revision and environment copy
        /// remain server-owned and atomic either way.
        #[operand(json, default = None)]
        pub template: Option<ProfileReq>,
        /// Copy the source's write-only environment; ignored when `environment` is present.
        #[operand(default = false)]
        pub copy_environment: bool,
        /// Explicit write-only environment composition for the clone.
        #[operand(json, default = None)]
        pub environment: Option<CloneProfileEnvironmentReq>,
    }

    pub type Output = ProfileView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod create {
    use super::prelude::*;

    /// Create a named session-launch profile.
    #[operation(
    id = "profiles.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles create",
)]
    pub struct Create;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile's name.
        #[operand(positional)]
        pub name: String,
        #[operand(default = String::new())]
        pub description: String,
        /// Agent runtime this profile launches (e.g. `claude`, `codex`).
        pub agent_kind: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub model: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub effort: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub protocol: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub mode: String,
        #[operand(default = String::from("interactive"))]
        pub class: String,
        #[operand(default = false)]
        pub strict: bool,
        #[operand(default = false)]
        pub env_clear: bool,
        pub ambient_allowlist: Vec<String>,
        pub idle_archive_secs: Option<i64>,
        #[operand(default = 0)]
        pub max_concurrent: i64,
        pub turn_budget: Option<i64>,
        #[operand(default = String::from("weaver"))]
        pub prelude: String,
        /// Organization-owned instructions appended to this profile's opening
        /// prompt for every launch origin.
        #[operand(default = String::new())]
        pub instructions: String,
        #[operand(default = false)]
        pub restricted: bool,
        /// Repositories for which Loom may broker a short-lived GitHub App
        /// token.
        pub github_repositories: Vec<String>,
        /// Provider-specific fallback permissions.
        pub runtime_permissions: Vec<String>,
        /// Provider-neutral MCP selection: `none`, `all`, or `groups`.
        #[operand(json, default = McpAccess::default())]
        pub mcp_access: McpAccess,
    }

    pub type Output = ProfileView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod delete {
    use super::prelude::*;

    /// Permanently delete a named launch profile.
    #[operation(
    id = "profiles.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "profiles delete",
    cli_alias = "rm",
)]
    pub struct Delete;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile's name.
        #[operand(positional)]
        pub name: String,
    }

    pub type Output = ProfileDeleteResult;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod effective {
    use super::prelude::*;

    /// Resolve one profile's exact non-secret policy — MCP snapshot, runtime
    /// permissions, and MCP server processes — without launching a session.
    #[operation(
    id = "profiles.effective",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles effective",
)]
    pub struct Effective;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile's name.
        #[operand(positional)]
        pub name: String,
    }

    pub type Output = EffectiveProfileView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod env {
    //! One named profile's write-only environment variables.

    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one profile's write-only environment variable.
        #[operation(
    id = "profiles.env.delete",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles env delete",
    cli_alias = "rm",
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The owning profile's name.
            #[operand(positional)]
            pub profile: String,
            /// The variable name.
            #[operand(positional)]
            pub name: String,
        }

        pub type Output = ProfileView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set {
        use super::prelude::*;

        /// Set one profile's write-only environment variable — a literal value or a
        /// GCP Secret Manager reference. Exactly one of the two is required.
        #[operation(
    id = "profiles.env.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles env set",
)]
        pub struct Set;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The owning profile's name.
            #[operand(positional)]
            pub profile: String,
            /// The variable name.
            #[operand(positional)]
            pub name: String,
            /// A write-only literal.
            pub value: Option<String>,
            /// A GCP Secret Manager version resource, resolved only at launch or
            /// respawn.
            pub secret_ref: Option<String>,
        }

        pub type Output = ProfileView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod get {
    use super::prelude::*;

    /// Show one named launch profile. Secret environment values are never
    /// returned.
    #[operation(
    id = "profiles.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles get",
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile's name.
        #[operand(positional)]
        pub name: String,
    }

    pub type Output = ProfileView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod list {
    use super::prelude::*;

    /// List named launch profiles. Secret environment values are never
    /// returned.
    #[operation(
    id = "profiles.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "profiles list",
    cli_alias = "ls",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<ProfileView>;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod update {
    use super::prelude::*;

    /// Replace a named session-launch profile's policy.
    #[operation(
    id = "profiles.update",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles update",
)]
    pub struct Update;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The profile's name.
        #[operand(positional)]
        pub name: String,
        #[operand(default = String::new())]
        pub description: String,
        /// Agent runtime this profile launches (e.g. `claude`, `codex`).
        pub agent_kind: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub model: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub effort: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub protocol: String,
        /// Blank uses the runtime's own default.
        #[operand(default = String::new())]
        pub mode: String,
        #[operand(default = String::from("interactive"))]
        pub class: String,
        #[operand(default = false)]
        pub strict: bool,
        #[operand(default = false)]
        pub env_clear: bool,
        pub ambient_allowlist: Vec<String>,
        pub idle_archive_secs: Option<i64>,
        #[operand(default = 0)]
        pub max_concurrent: i64,
        pub turn_budget: Option<i64>,
        #[operand(default = String::from("weaver"))]
        pub prelude: String,
        /// Organization-owned instructions appended to this profile's opening
        /// prompt for every launch origin.
        #[operand(default = String::new())]
        pub instructions: String,
        #[operand(default = false)]
        pub restricted: bool,
        /// Repositories for which Loom may broker a short-lived GitHub App
        /// token.
        pub github_repositories: Vec<String>,
        /// Provider-specific fallback permissions.
        pub runtime_permissions: Vec<String>,
        /// Provider-neutral MCP selection: `none`, `all`, or `groups`.
        #[operand(json, default = McpAccess::default())]
        pub mcp_access: McpAccess,
        /// Optimistic-concurrency guard: rejects a stale edit with a 409 and the
        /// current profile instead of silently overwriting it.
        pub expected_revision: Option<i64>,
    }

    pub type Output = ProfileView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <effective::Effective as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <clone::Clone as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "profiles",
        label: "Launch profiles",
        operations: OPERATIONS,
    }
}
