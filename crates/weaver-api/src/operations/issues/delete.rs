use super::prelude::*;

/// Permanently delete one or more work items atomically.
#[operation(
    id = "issues.delete",
    actor = SessionSelf,
    scope = Repository,
    risk = Destructive,
    grants = ["loom/issues/write@v1"],
    cli = "issues delete",
    cli_alias = "rm",
    mcp = "loom_issue::delete",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// One or more Loom work-item ids. Applied atomically: either every id
    /// succeeds or none does.
    #[operand(positional)]
    pub ids: Vec<i64>,
    #[operand(context)]
    pub repo_root: String,
}

pub type Output = IssueActionsResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
