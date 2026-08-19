use super::prelude::*;

/// List automation-triggered runs (GitHub Actions / ops / Grafana
/// deliveries): their status, launched session, and outcome.
///
/// Available to `User` actors (`user_grant_allows` in `crates/loom/src/web/auth.rs`).
/// This is an operator observability read for `Admin`/`User` actors only.
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

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
