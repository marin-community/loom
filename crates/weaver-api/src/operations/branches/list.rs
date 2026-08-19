use super::prelude::*;

/// List every branch loom is tracking.
///
/// Unfiltered and fleet-wide, like `sessions.list`: `grant_allows` in
/// `crates/loom/src/web/auth.rs` has always let a session credential read
/// bare `GET /branches`, so this stays `scope = Global` rather than
/// restricting the caller to its own branch.
#[operation(
    id = "branches.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/branches/read@v1"],
    cli = "branches list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<BranchView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
