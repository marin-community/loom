use super::prelude::*;

/// List and search visible sessions.
#[operation(
    id = "sessions.list",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions list",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Case-insensitive search over title, goal, branch, and tags.
    #[operand(default = String::new())]
    pub q: String,
    /// Widen the search to include recently archived sessions.
    #[operand(default = false)]
    pub history: bool,
    /// Return only archived sessions (the History view).
    #[operand(default = false)]
    pub archived_only: bool,
    /// Filter by lifecycle status.
    #[operand(json, default = None)]
    pub status: Option<SessionSearchStatus>,
    /// Filter by attention level.
    #[operand(json, default = None)]
    pub attention: Option<SessionSearchAttention>,
    /// Filter by who created the session, relative to the caller.
    #[operand(json, default = None)]
    pub creator: Option<SessionCreatorFilter>,
    /// Include automation-class sessions.
    ///
    /// Defaults to including them, which is what a fleet listing means by
    /// "every session". `loom ps` passes `false` for an interactive-only inventory.
    #[operand(default = true)]
    pub automation: bool,
    /// Include engine-managed warm sessions.
    ///
    /// An operator inventory escape hatch, refused to anything but a human
    /// credential: normal fleet and survey callers must not see a watcher's own
    /// infrastructure, because a watch that can see its own warm session can
    /// recurse into it.
    #[operand(default = false)]
    pub managed: bool,
}

pub type Output = Vec<SessionView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
