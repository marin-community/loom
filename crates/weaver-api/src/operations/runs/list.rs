use super::prelude::*;

/// List automation-triggered runs (GitHub Actions / ops / Grafana
/// deliveries): their status, launched session, and outcome.
///
/// `actor = User`: `GET` is unconditionally allowed for a `User` grant
/// (`user_grant_allows` in `crates/loom/src/web/auth.rs`), and the handler
/// itself lets `Admin`/`User` see every run unfiltered — an operator
/// observability read, not something a session credential has ever reached
/// (`/runs` is absent from the `Grant::Session` allowlist).
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
