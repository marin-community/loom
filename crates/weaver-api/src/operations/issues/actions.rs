//! The general bulk form: one action applied atomically to a set of work items.
//!
//! This operation is why the previous registry could not describe its own
//! surface. `IssueAction` is an internally-tagged union, and the old
//! `ArgumentKind` vocabulary had five scalar kinds — so this endpoint was marked
//! "custom" and left out. Nothing about it is special; it was simply unwritable.
//! Deriving the schema from the real type removes the ceiling, so it registers
//! like anything else.

use super::prelude::*;

/// Apply one action atomically to a set of work items.
#[operation(
    id = "issues.actions",
    actor = SessionSelf,
    scope = Repository,
    risk = Write,
    grants = ["loom/issues/write@v1"],
    cli = "issues actions",
    mcp = "loom_issue::actions",
    render = custom,
)]
pub struct Actions;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The work items to act on. Either every id succeeds or none does.
    #[operand(long = "id")]
    pub ids: Vec<i64>,
    /// The action to apply — `close`, `reopen`, `delete`, `tag`, or `untag`.
    /// On the command line this takes a JSON object, because a tagged union is
    /// not a flag.
    #[operand(json)]
    pub action: IssueAction,
    #[operand(context)]
    pub repo_root: String,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            action: IssueAction::Close,
            repo_root: String::new(),
        }
    }
}

pub type Output = IssueActionsResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
