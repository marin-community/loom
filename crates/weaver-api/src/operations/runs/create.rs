use super::prelude::*;

/// Reserve and launch (or idempotently return) one automation-triggered
/// session: the entry point GitHub Actions, ops scripts, and Grafana alerts
/// call through their federated automation credential.
///
/// `actor = Internal`: the only real caller is the runtime itself, presenting
/// an automation bearer token minted by `POST /api/auth/automation-token` (see
/// `.github/workflows/loom-issue.yml`). `crates/loom/src/web/automation.rs`'s
/// `run_identity` also lets a plain `Admin`/`User` grant name any profile —
/// there is no CLI command or dashboard control that exercises that path
/// today, so the more restrictive `Internal` is the honest description of
/// this operation's actual surface; `Admin` still reaches it unconditionally.
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

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
