use super::prelude::*;

/// Replace an existing custom agent's definition. The name is immutable; a
/// builtin or unknown name is rejected.
///
/// Operator-only, same reasoning as `agents.custom.create`.
#[operation(
    id = "agents.custom.update",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "agents custom update",
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The custom agent's name.
    #[operand(positional)]
    pub name: String,
    /// The display name shown in the agent picker.
    #[operand(default = String::new())]
    pub label: String,
    /// Shell run in the worktree before launch.
    #[operand(default = String::new())]
    pub setup: String,
    /// The fresh-session launch command; the goal is appended as an
    /// argument.
    #[operand(default = String::new())]
    pub launch: String,
    /// The adopt/resume command (no goal). Blank reuses `launch`.
    #[operand(default = String::new())]
    pub resume: String,
    /// Whether the agent fires loom's lifecycle hooks.
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
