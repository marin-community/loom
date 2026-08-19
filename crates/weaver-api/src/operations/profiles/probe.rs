use super::prelude::*;

/// Validate a profile's effective MCP policy and report any resolution
/// errors, without launching a session.
#[operation(
    id = "profiles.probe",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "profiles probe",
)]
pub struct Probe;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile's name.
    #[operand(positional)]
    pub name: String,
}

pub type Output = ProfileProbeView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
