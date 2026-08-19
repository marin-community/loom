use super::prelude::*;

/// Set one free-form tag on a work item.
#[operation(
    id = "issues.tags.set",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues tag set",
    mcp = "loom_issue::tag_set",
    render = custom,
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A Loom work-item id.
    #[operand(positional)]
    pub id: i64,
    /// The tag key.
    #[operand(positional)]
    pub key: String,
    /// The tag value. Clear a tag with `issues tag delete` rather than setting
    /// an empty value.
    #[operand(positional)]
    pub value: String,
    /// One-line reason accompanying the tag.
    #[operand(default = String::new())]
    pub note: String,
    #[operand(context)]
    pub repo_root: String,
}

pub type Output = IssueView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
