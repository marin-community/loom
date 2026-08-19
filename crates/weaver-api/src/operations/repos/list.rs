use super::prelude::*;

/// List the registered managed repos (the clone allowlist).
#[operation(
    id = "repos.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<RepoView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
