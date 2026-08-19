use super::prelude::*;

/// Fire a watch round now, in the daemon, and report its outcome. `dry_run`
/// stubs every mutating action — the iteration primitive, safe to repeat.
///
/// Operator-only, same reasoning as `watches.create`: manually firing a round
/// is a mutating `/watches/{id}/run` route, which `user_grant_allows` refuses
/// a plain `User` grant.
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
