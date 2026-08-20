//! Watches — periodic / triggered watch programs over the fleet.
//!
//! The operator + authoring surface over `weaver_core::watch`: register a
//! watch, arm/disarm it, fire a round on demand, and inspect its round
//! history.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod create {
    use super::prelude::*;

    /// Register a watch.
    ///
    /// Admin only. Watches are fleet-wide configuration.
    #[operation(
    id = "watches.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "watch add",
)]
    pub struct Create;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The watch's unique name.
        #[operand(positional)]
        pub name: String,
        /// The event-match predicate: `{cron|every|event|level|repo}`. Defaults to
        /// the program's declared trigger (register-mode manifest), or an empty
        /// predicate.
        #[operand(json, default = None)]
        pub trigger: Option<serde_json::Value>,
        /// The fleet query a round surveys: `{attention?, repo?}`.
        #[operand(json, default = None)]
        pub scope: Option<serde_json::Value>,
        /// `builtin:<name>` for a stock program, or an absolute path under
        /// `~/.weaver/watches/` for a custom one.
        pub program: Option<String>,
        /// Stock-program parameters (e.g. the judgement `prompt`).
        #[operand(json, default = None)]
        pub params: Option<serde_json::Value>,
        /// The granted capability set (the intervention ladder). `observe` is
        /// implicit; the rest are explicit grants.
        #[operand(json, default = None)]
        pub capabilities: Option<Vec<String>>,
        /// Automation-safe ACP launch profile used for agent judgements and warm
        /// sessions.
        pub profile: Option<String>,
        pub model: Option<String>,
        pub effort: Option<String>,
        pub cooldown_secs: Option<i64>,
        /// Whether the watch fires as soon as it is created. Omitted clients get
        /// the model default (disabled); the loom UI sends `true` so a watcher
        /// picked from the builtin registry is live without a separate manual
        /// enable.
        #[operand(json, default = None)]
        pub enabled: Option<bool>,
    }

    pub type Output = WatchView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod delete {
    use super::prelude::*;

    /// Remove a watch.
    ///
    /// Operator-only, same reasoning as `watches.create`.
    #[operation(
    id = "watches.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "watch rm",
)]
    pub struct Delete;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
    }

    pub type Output = WatchDeleteResult;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod get {
    use super::prelude::*;

    /// Inspect one watch by id or name.
    #[operation(
    id = "watches.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch get",
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
    }

    pub type Output = WatchView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod list {
    use super::prelude::*;

    /// List every registered watch: name, enabled, trigger, program, last outcome.
    #[operation(
    id = "watches.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch ls",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<WatchView>;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod programs {
    use super::prelude::*;

    /// List the builtin watch programs that ship with loom.
    #[operation(
    id = "watches.programs",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch programs",
    view = View,
)]
    pub struct Programs;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<ProgramView>;

    /// CLI-only flags that never cross the wire: the full registry is always
    /// fetched, this only chooses what gets printed.
    #[derive(Debug, Clone, Default, View)]
    pub struct View {
        /// Print one program's embedded script source instead of the table, e.g.
        /// `--source builtin:archive-merged`.
        pub source: Option<String>,
    }

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod run {
    use super::prelude::*;

    /// Fire a watch round now, in the daemon, and report its outcome. `dry_run`
    /// stubs every mutating action — the iteration primitive, safe to repeat.
    ///
    /// Operator-only, same reasoning as `watches.create`.
    #[operation(
    id = "watches.run",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "watch run",
)]
    pub struct Run;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
        /// Simulate: every mutating action is stubbed and logged as "would do X",
        /// nothing is performed.
        #[operand(default = false)]
        pub dry_run: bool,
    }

    pub type Output = WatchRunResult;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod runs {
    use super::prelude::*;

    /// Show a watch's round history: time, trigger reason, outcome, summary, and
    /// the captured stdout/stderr/exit status of each round.
    #[operation(
    id = "watches.runs",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch runs",
    cli_alias = "logs",
)]
    pub struct Runs;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
        /// How many recent rounds to return; defaults to 50, clamped to 1000.
        pub limit: Option<i64>,
    }

    pub type Output = Vec<WatchRunView>;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod update {
    use super::prelude::*;

    /// Update a watch's settings, optionally arm or disarm it via the `enabled` field.
    ///
    /// Operator-only for the same reason as `watches.create` — a `User` grant is
    /// refused on every mutating `/watches/{id}` route.
    #[operation(
    id = "watches.update",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "watch update",
)]
    pub struct Update;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
        /// Arm (`true`) or disarm (`false`) the watch.
        #[operand(json, default = None)]
        pub enabled: Option<bool>,
        /// The event-match predicate: `{cron|every|event|level|repo}`. Setting the
        /// program without an explicit trigger re-evaluates the new program's
        /// register-mode manifest.
        #[operand(json, default = None)]
        pub trigger: Option<serde_json::Value>,
        /// The fleet query a round surveys: `{attention?, repo?}`.
        #[operand(json, default = None)]
        pub scope: Option<serde_json::Value>,
        /// `builtin:<name>` for a stock program, or an absolute path under
        /// `~/.weaver/watches/` for a custom one.
        pub program: Option<String>,
        /// Stock-program parameters (e.g. the judgement `prompt`).
        #[operand(json, default = None)]
        pub params: Option<serde_json::Value>,
        /// The granted capability set (the intervention ladder).
        #[operand(json, default = None)]
        pub capabilities: Option<Vec<String>>,
        /// Automation-safe ACP launch profile.
        pub profile: Option<String>,
        pub model: Option<String>,
        pub effort: Option<String>,
        pub cooldown_secs: Option<i64>,
    }

    pub type Output = WatchView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <programs::Programs as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <run::Run as Operation>::SPEC,
    <runs::Runs as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "watches",
        label: "Watches",
        operations: OPERATIONS,
    }
}
