use super::prelude::*;

/// Create a work item claimed by this session's branch.
#[operation(
    id = "issues.create",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues add",
    mcp = "loom_issue::add",
    render = custom,
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// One-line summary of the work.
    #[operand(positional)]
    pub title: String,
    /// Optional detail.
    #[operand(default = String::new())]
    pub body: String,
    /// Link the item to an existing GitHub issue number.
    pub github_issue: Option<i64>,
    #[operand(context)]
    pub branch: String,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
