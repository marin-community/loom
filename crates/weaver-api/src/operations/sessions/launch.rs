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

    // The fields below are what `loom sessions launch` actually sends. An
    // earlier draft of this contract omitted them, which would have quietly
    // downgraded every launch to profile defaults and dropped the staleness
    // guards — a launch is the operation where getting the configuration wrong
    // is least visible and most expensive.
    /// Explicit branch name instead of a generated one.
    pub name: Option<String>,
    /// Attach to a branch that already exists rather than creating one.
    pub existing_branch: Option<String>,
    /// A GitHub issue number to link the session to.
    pub github_issue: Option<i64>,
    /// Model override, when the profile's default is not wanted.
    pub model: Option<String>,
    /// Reasoning-effort override.
    pub effort: Option<String>,
    /// The resolved profile and per-launch overrides.
    ///
    /// Carries the agent, model, effort, and MCP access the caller previewed.
    #[operand(json, skip_cli)]
    pub selection: Option<LaunchSelection>,
    /// Files to seed the session's scratch directory with.
    #[operand(json, skip_cli)]
    pub scratch: Vec<ScratchUpload>,
    /// Optimistic-concurrency guards: the profile and resolver revisions the
    /// caller previewed against. A launch whose configuration changed underneath
    /// it is rejected rather than silently run with different settings.
    #[operand(skip_cli)]
    pub expected_profile_revision: Option<i64>,
    /// The resolver revision is a content hash, not a counter.
    #[operand(skip_cli)]
    pub expected_resolver_revision: Option<String>,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
