use super::prelude::*;

/// The fleet index: one compact row per visible session.
///
/// A reduced projection to keep responses compact. Full session context is
/// available separately via `sessions.get` to avoid accidentally fetching
/// the large projection when the compact view is all that is needed.
#[operation(
    id = "sessions.summary.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "sessions summaries",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Include archived rows alongside active work.
    #[operand(default = false)]
    pub archived: bool,
    /// Return only archived rows. Implies `archived`.
    #[operand(default = false)]
    pub archived_only: bool,
    /// Include automation-class sessions.
    #[operand(default = false)]
    pub automation: bool,
    /// Case-insensitive search over the same facets as fleet search.
    #[operand(default = String::new())]
    pub q: String,
    /// Filter by lifecycle status.
    #[operand(json, default = None)]
    pub status: Option<SessionSearchStatus>,
    /// Filter by attention level.
    #[operand(json, default = None)]
    pub attention: Option<SessionSearchAttention>,
    /// Filter by who created the session, relative to the caller.
    #[operand(json, default = None)]
    pub creator: Option<SessionCreatorFilter>,
}

pub type Output = Vec<SessionSummaryView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
