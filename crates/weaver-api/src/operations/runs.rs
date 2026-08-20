//! Automation-triggered runs — GitHub Actions, ops scripts, and Grafana
//! alerts dispatching a session through a federated automation credential.
//!
//! A `weaver_core::runs::Run` is one delivery attempt through
//! `POST /api/auth/automation-token`-minted credentials, tracked for
//! idempotent redelivery and operator observability. `runs.create` is gated
//! to the runtime (`actor = Internal`), while the read side is an operator
//! diagnostic surface (`actor = User`).

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod create {
    use super::prelude::*;

    /// Reserve and launch (or idempotently return) one automation-triggered
    /// session: the entry point GitHub Actions, ops scripts, and Grafana alerts
    /// call through their federated automation credential.
    ///
    /// `actor = Internal`: the runtime is the only real caller, presenting an
    /// automation bearer token minted by `auth.automation_token`. `Admin` can
    /// still reach it directly.
    #[operation(
    id = "runs.create",
    actor = Internal,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Create;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Launch profile the run executes under.
        pub profile: String,
        /// Caller-selected durable key. Verified GitHub callers may leave this
        /// blank to use repository/run/attempt, or provide a bounded deterministic
        /// key that Loom namespaces to the verified identity.
        #[operand(default = String::new())]
        pub idempotency_key: String,
        /// Trigger source: `actions`, `ops`, or `grafana`.
        #[operand(default = String::from("actions"))]
        pub source: String,
        /// The originating watch, when this run was triggered by a watch program.
        pub watch_id: Option<String>,
        /// Stable conversation route for related deliveries. Each idempotency key
        /// remains a distinct run; channel deliveries reuse one live ACP session.
        pub channel: Option<String>,
        /// The Slack thread this delivery was announced in, so the session it
        /// lands on can reply there.
        #[operand(json, default = None)]
        pub slack: Option<SlackThreadRef>,
        /// The session to launch.
        #[operand(json)]
        pub session: CreateReq,
    }

    pub type Output = RunView;
}

pub mod get {
    use super::prelude::*;

    /// Inspect one automation-triggered run by id.
    ///
    /// `actor = User`, matching `runs.list`.
    #[operation(
    id = "runs.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "runs get",
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The run id.
        #[operand(positional)]
        pub id: String,
    }

    pub type Output = RunView;
}

pub mod list {
    use super::prelude::*;

    /// List automation-triggered runs (GitHub Actions / ops / Grafana
    /// deliveries): their status, launched session, and outcome.
    ///
    /// An operator observability read: `actor = User`, so `Admin`/`User` only.
    #[operation(
    id = "runs.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "runs list",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<RunView>;
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "runs",
        label: "Automation runs",
        operations: OPERATIONS,
    }
}
