use super::prelude::*;

/// Define a new custom agent — a name, a label, and a shell command per
/// launch stage — so it appears in the picker beside the builtin
/// `claude`/`codex` without a code change.
///
/// Operator-only: a `User` grant is explicitly refused on every mutating
/// `/agents/custom` route (`user_grant_allows` in
/// `crates/loom/src/web/auth.rs`), so only `Admin` may create one.
#[operation(
    id = "agents.custom.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "agents custom create",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The new agent's unique id. Must not shadow a builtin (`claude`,
    /// `codex`) or the retired `concierge` name.
    #[operand(positional)]
    pub name: String,
    /// The display name shown in the agent picker.
    #[operand(default = String::new())]
    pub label: String,
    /// Shell run in the worktree before launch — the "installing hooks"
    /// stage.
    #[operand(default = String::new())]
    pub setup: String,
    /// The fresh-session launch command; the goal is appended as an
    /// argument.
    #[operand(default = String::new())]
    pub launch: String,
    /// The adopt/resume command (no goal). Blank reuses `launch`.
    #[operand(default = String::new())]
    pub resume: String,
    /// Whether the agent fires loom's lifecycle hooks (working / idle /
    /// attention signals).
    #[operand(default = false)]
    pub reports_status: bool,
    /// Execution backend: `terminal` (the default) or `acp`.
    #[operand(default = String::new())]
    pub protocol: String,
}

pub type Output = CustomAgentsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
