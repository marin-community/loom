//! Server-wide runtime settings and the default profile's environment
//! compatibility facade.
//!
//! `loom config render-env`, `secret-names`, `push-secrets`, and `set` are
//! deliberately absent: they read/write `loom.toml` or the sqlite `settings`
//! table directly with no running server, so they are not operations. The
//! REST surface here — `settings.get`/`settings.patch` and the
//! `settings.env.*` facade they expose to operators — is.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod env {
    //! The protected `default` profile's environment — a flat name/value
    //! compatibility facade predating per-profile environment stores. See
    //! `loom_store::agent_env` for the storage this projects.

    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one variable from the default profile's environment. A missing
        /// name is not an error — the desired end state already holds.
        #[operation(
    id = "settings.env.delete",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings env delete",
    cli_alias = "rm",
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The variable name.
            #[operand(positional)]
            pub name: String,
        }

        pub type Output = Vec<AgentEnvVarView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod list {
        use super::prelude::*;

        /// List every variable in the default profile's environment. Unlike a named
        /// profile's environment metadata, values are returned in full.
        #[operation(
    id = "settings.env.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "settings env list",
    cli_alias = "ls",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {}

        pub type Output = Vec<AgentEnvVarView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set {
        use super::prelude::*;

        /// Upsert one variable in the default profile's environment. The value is
        /// free-form; the name is validated as a shell identifier so it cannot
        /// corrupt the launch script that exports it.
        #[operation(
    id = "settings.env.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings env set",
)]
        pub struct Set;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The variable name — a POSIX-portable shell identifier.
            #[operand(positional)]
            pub name: String,
            /// The value to store.
            #[operand(positional)]
            pub value: String,
        }

        pub type Output = Vec<AgentEnvVarView>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod get {
    use super::prelude::*;

    /// Every registered runtime setting and its effective value.
    ///
    /// `SessionSelf` because an agent may read the configuration it runs under —
    /// `GET /settings` was reachable by a session credential before this was a
    /// declaration. Writing one is `settings.patch`, which is `Admin`. The grant is
    /// the session read grant: there is no narrower capability a session can hold,
    /// and minting one nothing issues would deny the read outright.
    #[operation(
    id = "settings.get",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "settings get",
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = SettingsEnvelope;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod patch {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::prelude::*;

    /// Apply setting changes. A `null` value clears a key back to its default.
    #[operation(
    id = "settings.patch",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "settings patch",
)]
    pub struct Patch;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Dotted setting key to new value; `null` clears that key back to its
        /// default.
        ///
        /// A value may be a string, a boolean, or a number. Settings are *stored* as
        /// strings, but a caller naturally writes the setting's own type — `false`
        /// for `auth.trust_loopback`, `300` for a `_secs` key — and requiring
        /// `"false"` would make this operation stricter than the route it replaces
        /// for no benefit. Coercion happens once, server-side; anything else (an
        /// array, an object) is rejected by key.
        #[operand(json, default = BTreeMap::new())]
        pub changes: BTreeMap<String, Option<Value>>,
    }

    pub type Output = SettingsEnvelope;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <patch::Patch as Operation>::SPEC,
    <env::list::List as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "settings",
        label: "Runtime settings",
        operations: OPERATIONS,
    }
}
