use super::prelude::*;

/// Create an unclaimed repository backlog item.
#[operation(
    id = "issues.backlog.create",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues backlog add",
    mcp = "loom_issue::backlog_add",
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
    pub repo_root: String,
    /// The branch that filed this item, for provenance.
    ///
    /// The branch *name*, not its id — this is compared against `branch.branch`
    /// when the CLI decides whether an item was delegated by the current branch.
    #[operand(context = "branch_name")]
    pub source_branch: Option<String>,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
