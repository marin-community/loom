//! Watches — periodic / triggered watch programs over the fleet.
//!
//! Operator and authoring operations over `weaver_core::watch`: register a
//! watch, arm/disarm it, fire a round on demand, and inspect its round
//! history.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod create {
    use super::prelude::*;

    /// Register a watch.
    ///
    /// Watches are fleet-wide configuration, not per-session.
    #[operation(id = "watches.create", actor = Admin, scope = Global, risk = Write,
                cli = "watch add", render = custom)]
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
        /// Agent prompt, launch profile, repository and allowed Slack channels.
        #[operand(json, default = None)]
        pub agent: Option<weaver_core::schedule::AgentTarget>,
        #[operand(json, default = None)]
        pub misfire_policy: Option<weaver_core::schedule::MisfirePolicy>,
        pub late_grace_secs: Option<i64>,
        pub run_timeout_secs: Option<i64>,
        /// Whether the watch fires as soon as it is created. Omitted clients get
        /// the model default (disabled); the loom UI sends `true` so a watcher
        /// picked from the builtin registry is live without a separate manual
        /// enable.
        #[operand(json, default = None)]
        pub enabled: Option<bool>,
    }

    pub type Output = WatchView;
}

pub mod delete {
    use super::prelude::*;

    /// Remove a watch.
    #[operation(id = "watches.delete", actor = Admin, scope = Global, risk = Destructive,
                cli = "watch rm")]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
    }

    pub type Output = WatchDeleteResult;
}

pub mod get {
    use super::prelude::*;

    /// Inspect one watch by id or name.
    #[operation(id = "watches.get", actor = User, scope = Global, risk = Read, cli = "watch get",
                render = custom)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
    }

    pub type Output = WatchView;
}

pub mod list {
    use super::prelude::*;

    /// List every registered watch: name, enabled, trigger, program, last outcome.
    #[operation(id = "watches.list", actor = User, scope = Global, risk = Read, cli = "watch ls",
                render = custom)]
    pub struct Input {}

    pub type Output = Vec<WatchView>;
}

pub mod programs {
    use super::prelude::*;

    /// List the builtin watch programs that ship with loom.
    ///
    /// No `cli`: `loom watch programs` takes `--source <name>`, which is a
    /// lookup in the returned list that can miss, and a `Render` returns a
    /// `String` with no way to say "that name does not exist". The command
    /// stays hand-written so a typo still exits non-zero; it prints the table
    /// through this operation's renderer.
    #[operation(id = "watches.programs", actor = User, scope = Global, risk = Read,
                render = custom)]
    pub struct Input {}

    pub type Output = Vec<ProgramView>;
}

pub mod run {
    use super::prelude::*;

    /// Fire a watch round now, in the daemon, and report its outcome. `dry_run`
    /// stubs every mutating action — the iteration primitive, safe to repeat.
    #[operation(id = "watches.run", actor = Admin, scope = Global, risk = Write, cli = "watch run")]
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
}

pub mod runs {
    use super::prelude::*;

    /// Show a watch's round history: time, trigger reason, outcome, summary, and
    /// the captured stdout/stderr/exit status of each round.
    #[operation(id = "watches.runs", actor = User, scope = Global, risk = Read, cli = "watch runs",
                cli_alias = "logs", view = View, render = custom)]
    pub struct Input {
        /// Watch id or name.
        #[operand(positional)]
        pub key: String,
        /// How many recent rounds to return; defaults to 50, clamped to 1000.
        pub limit: Option<i64>,
    }

    pub type Output = Vec<WatchRunView>;

    /// CLI-only flags that never leave the client: the whole history is fetched
    /// either way, this only chooses how much of each round gets printed.
    #[derive(Debug, Clone, Default, Deserialize, View)]
    pub struct View {
        /// Print each round's summary and the actions it took, not one row per
        /// round.
        pub verbose: bool,
    }
}

pub mod update {
    use super::prelude::*;

    /// Update a watch's settings, optionally arm or disarm it via the `enabled` field.
    #[operation(id = "watches.update", actor = Admin, scope = Global, risk = Write,
                cli = "watch update")]
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
        /// Agent prompt, launch profile, repository and allowed Slack channels.
        #[operand(json, default = None)]
        pub agent: Option<weaver_core::schedule::AgentTarget>,
        #[operand(json, default = None)]
        pub misfire_policy: Option<weaver_core::schedule::MisfirePolicy>,
        pub late_grace_secs: Option<i64>,
        pub run_timeout_secs: Option<i64>,
    }

    pub type Output = WatchView;
}

pub mod preview {
    use super::prelude::*;
    /// Validate a schedule and show its next five fire times.
    #[operation(id = "watches.preview", actor = User, scope = Global, risk = Read,
                cli = "watch preview")]
    pub struct Input {
        #[operand(json)]
        pub trigger: serde_json::Value,
    }
    pub type Output = Vec<String>;
}

pub mod occurrences {
    use super::prelude::*;
    /// Inspect durable agent occurrences, including launch and session identifiers.
    #[operation(id = "watches.occurrences", actor = User, scope = Global, risk = Read,
                cli = "watch occurrences")]
    pub struct Input {
        #[operand(positional)]
        pub key: String,
        pub limit: Option<i64>,
    }
    pub type Output = Vec<weaver_core::occurrence::Occurrence>;
}

pub mod state {
    use super::prelude::*;
    /// Read or atomically replace the active scheduled agent's persistent state.
    #[operation(id = "watches.state", actor = SessionSelf, scope = Branch, risk = Write,
                grants = ["loom/watches/state@v1"], cli = "watch state")]
    pub struct Input {
        #[operand(context)]
        pub branch: String,
        #[operand(json, default = None)]
        pub value: Option<serde_json::Value>,
        pub expected_version: Option<i64>,
    }
    pub type Output = serde_json::Value;
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    get::SPEC,
    programs::SPEC,
    create::SPEC,
    update::SPEC,
    delete::SPEC,
    run::SPEC,
    runs::SPEC,
    preview::SPEC,
    occurrences::SPEC,
    state::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "watches",
        label: "Watches",
        operations: OPERATIONS,
    }
}
