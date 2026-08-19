use super::prelude::*;

/// Close one or more work items atomically.
#[operation(
    id = "issues.close",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues close",

    mcp = "loom_issue::close",
    render = custom,
)]
pub struct Close;

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
