use super::prelude::*;

/// Inspect one work item and the status of the branch working it.
#[operation(
    id = "issues.get",
    actor = SessionSelf,
    scope = Repository,
    risk = Read,
    grants = ["loom/issues/read@v1"],
    cli = "issues get",
    cli_alias = "show",
    mcp = "loom_issue::get",
    render = custom,
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A Loom work-item id.
    #[operand(positional)]
    pub id: i64,
    #[operand(context)]
    pub repo_root: String,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
