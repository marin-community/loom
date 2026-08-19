use super::prelude::*;

/// Register a watch.
///
/// Operator-only: a `User` grant is explicitly refused every mutating
/// `/watches` route (`user_grant_allows` in `crates/loom/src/web/auth.rs`), so
/// only `Admin` may create one — this is fleet configuration, not a per-branch
/// action a signed-in user takes on their own behalf.
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
