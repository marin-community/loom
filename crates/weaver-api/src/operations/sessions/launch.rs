use super::prelude::*;

/// Launch a child session from a task or claimed work item.
#[operation(
    id = "sessions.launch",
    actor = SessionSelf,
    scope = Global,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "launch",
)]
pub struct Launch;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// One-line task label for the new session.
    #[operand(positional)]
    pub title: String,
    /// Detailed goal for the new session; defaults to the task label.
    pub goal: Option<String>,
    /// A managed repository (GitHub `owner/name`) to launch against.
    pub repo: Option<String>,
    /// Local worktree path to fork the session's worktree from, when not
    /// launching against a managed `repo`.
    #[operand(default = String::new())]
    pub cwd: String,
    /// Base branch or ref to fork from.
    pub base: Option<String>,
    /// Agent runtime to launch; blank uses the profile's default.
    pub agent: Option<String>,
    /// Named launch profile; blank selects `default`.
    pub profile: Option<String>,
    /// A pre-existing Loom backlog item to claim for this session.
    pub claim_issue: Option<i64>,
    /// An existing GitHub issue number to seed the session from.
    pub issue: Option<i64>,
    /// The branch of the launching session, when this is an agent-delegated
    /// launch. Filled from the caller's own branch; a human/dashboard launch
    /// leaves it unset.
    #[operand(context = "branch")]
    pub parent_branch: Option<String>,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
