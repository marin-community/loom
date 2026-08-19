//! `issues.list` — the reference shape for every operation in the registry.
//!
//! Read top to bottom, this is the whole contract: who may call it, what it
//! accepts, what it returns, and how it prints. REST, the CLI, and MCP are all
//! generated from what is here; none of them adds arguments of its own.

use super::prelude::*;

/// List current-session and repository work items.
#[operation(
    id = "issues.list",
    actor = SessionSelf,
    scope = Repository,
    risk = Read,
    grants = ["loom/issues/read@v1"],
    cli = "issues list",
    cli_alias = "ls",
    mcp = "loom_issue::list",
    view = View,
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub repo_root: String,
    /// Include closed work items.
    #[operand(default = false)]
    pub all: bool,
}

pub type Output = Vec<IssueView>;

/// Presentation flags. These never cross the wire — they choose how the result
/// is printed, which is why they live here rather than in `Input`.
#[derive(Debug, Clone, Default, View)]
pub struct View {
    /// Show every work item in the repository, uncapped.
    pub repo: bool,
    /// Show only the items claimed by this branch.
    pub mine: bool,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Repository(&self.repo_root)
    }
}
