use super::prelude::*;

/// Patch a watch: every mutable field optional, including `enabled` (the
/// arm/disarm toggle).
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
