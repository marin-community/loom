use super::prelude::*;

/// Recently-used repositories, most recent first — the launch flow's repo
/// picker.
#[operation(
    id = "repos.recent",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos recent",
)]
pub struct Recent;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Maximum repos to return (1-50); defaults to 10.
    pub limit: Option<i64>,
}

pub type Output = Vec<RecentRepoView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
