//! Agent runtimes: the picker list (builtins + operator-defined custom
//! agents) and the custom-agent editor's CRUD.
//!
//! `agents.list` is a plain fleet-wide read, `actor = SessionSelf`. Defining,
//! editing, and removing a custom agent are different in kind — `actor =
//! Admin` — since this is fleet configuration (which runtimes exist at all),
//! not a per-branch action a signed-in user takes on their own behalf,
//! exactly like `watches.create`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod custom {
    //! Operator-defined custom agent CRUD.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Define a new custom agent — a name, a label, and a shell command per
        /// launch stage — so it appears in the picker beside the builtin
        /// `claude`/`codex` without a code change.
        ///
        /// Operator-only: `actor = Admin`, so only `Admin` may create one.
        #[operation(
    id = "agents.custom.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "agents custom create",
)]
        pub struct Create;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The new agent's unique id. Must not shadow a builtin (`claude`,
            /// `codex`) or the retired `concierge` name.
            #[operand(positional)]
            pub name: String,
            /// The display name shown in the agent picker.
            #[operand(default = String::new())]
            pub label: String,
            /// Shell run in the worktree before launch — the "installing hooks"
            /// stage.
            #[operand(default = String::new())]
            pub setup: String,
            /// The fresh-session launch command; the goal is appended as an
            /// argument.
            #[operand(default = String::new())]
            pub launch: String,
            /// The adopt/resume command (no goal). Blank reuses `launch`.
            #[operand(default = String::new())]
            pub resume: String,
            /// Whether the agent fires loom's lifecycle hooks (working / idle /
            /// attention signals).
            #[operand(default = false)]
            pub reports_status: bool,
            /// Execution backend: `terminal` (the default) or `acp`.
            #[operand(default = String::new())]
            pub protocol: String,
        }

        pub type Output = CustomAgentsView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod delete {
        use super::prelude::*;

        /// Remove a custom agent. Removing an absent name is a no-op. Sessions
        /// already launched with it are unaffected.
        ///
        /// Operator-only, same reasoning as `agents.custom.create`.
        #[operation(
    id = "agents.custom.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "agents custom delete",
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The custom agent's name.
            #[operand(positional)]
            pub name: String,
        }

        pub type Output = CustomAgentsView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod update {
        use super::prelude::*;

        /// Replace an existing custom agent's definition. The name is immutable; a
        /// builtin or unknown name is rejected.
        ///
        /// Operator-only, same reasoning as `agents.custom.create`.
        #[operation(
    id = "agents.custom.update",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "agents custom update",
)]
        pub struct Update;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The custom agent's name.
            #[operand(positional)]
            pub name: String,
            /// The display name shown in the agent picker.
            #[operand(default = String::new())]
            pub label: String,
            /// Shell run in the worktree before launch.
            #[operand(default = String::new())]
            pub setup: String,
            /// The fresh-session launch command; the goal is appended as an
            /// argument.
            #[operand(default = String::new())]
            pub launch: String,
            /// The adopt/resume command (no goal). Blank reuses `launch`.
            #[operand(default = String::new())]
            pub resume: String,
            /// Whether the agent fires loom's lifecycle hooks.
            #[operand(default = false)]
            pub reports_status: bool,
            /// Execution backend: `terminal` (the default) or `acp`.
            #[operand(default = String::new())]
            pub protocol: String,
        }

        pub type Output = CustomAgentsView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod list {
    use super::prelude::*;

    /// List available agent runtimes: builtins, operator-defined custom agents,
    /// and the configured default.
    #[operation(
    id = "agents.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/agents/read@v1"],
    cli = "agents list",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = AgentsView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod oneshot {
    use super::prelude::*;

    /// Run a one-shot ACP prompt through a registered agent runtime and return its
    /// text — the judgement-call primitive watch programs call.
    ///
    /// `actor = User`: a signed-in user may call this.
    ///
    /// `risk = ExternalWrite`: without a `profile` the prompt runs with no branch
    /// or session sandbox and no automation-safe policy constraining it — the
    /// same blast radius as `shell.terminal`, just LLM-issued instructions rather
    /// than operator-typed ones.
    #[operation(
    id = "agents.oneshot",
    actor = User,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
)]
    pub struct Oneshot;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The prompt to run.
        #[operand(positional)]
        pub prompt: String,
        /// Optional launch profile. When set, its runtime and policy are
        /// authoritative; model and effort remain optional per-call overrides.
        #[operand(default = String::new())]
        pub profile: String,
        /// Registered ACP runtime. Empty keeps the built-in Claude runtime.
        #[operand(default = String::new())]
        pub agent: String,
        /// Model override advertised by the runtime; empty keeps its ACP default.
        #[operand(default = String::new())]
        pub model: String,
        /// Reasoning effort override advertised by the runtime; empty keeps its
        /// ACP default.
        #[operand(default = String::new())]
        pub effort: String,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct Output {
        /// `null` when the adapter is absent or fails — callers degrade to their
        /// own deterministic fallback rather than seeing an error.
        pub output: Option<String>,
    }

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <custom::create::Create as Operation>::SPEC,
    <custom::update::Update as Operation>::SPEC,
    <custom::delete::Delete as Operation>::SPEC,
    <oneshot::Oneshot as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "agents",
        label: "Agent runtimes",
        operations: OPERATIONS,
    }
}
