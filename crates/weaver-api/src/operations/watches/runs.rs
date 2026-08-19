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
