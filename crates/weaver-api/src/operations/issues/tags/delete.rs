use super::prelude::*;

/// Remove one free-form tag from a work item.
#[operation(
    id = "issues.tags.delete",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues tag delete",
    cli_alias = "rm",
    mcp = "loom_issue::tag_delete",
    render = custom,
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A Loom work-item id.
    #[operand(positional)]
    pub id: i64,
    /// The tag key to remove.
    #[operand(positional)]
    pub key: String,
    #[operand(context)]
    pub repo_root: String,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
