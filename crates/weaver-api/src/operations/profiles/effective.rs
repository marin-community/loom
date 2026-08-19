use super::prelude::*;

/// Resolve one profile's exact non-secret policy — MCP snapshot, runtime
/// permissions, and MCP server processes — without launching a session.
#[operation(
    id = "profiles.effective",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles effective",
)]
pub struct Effective;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile's name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = EffectiveProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
