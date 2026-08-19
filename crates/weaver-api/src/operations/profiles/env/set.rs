use super::prelude::*;

/// Set one profile's write-only environment variable — a literal value or a
/// GCP Secret Manager reference. Exactly one of the two is required.
#[operation(
    id = "profiles.env.set",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles env set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The owning profile's name.
    #[operand(positional)]
    pub profile: String,
    /// The variable name.
    #[operand(positional)]
    pub name: String,
    /// A write-only literal.
    pub value: Option<String>,
    /// A GCP Secret Manager version resource, resolved only at launch or
    /// respawn.
    pub secret_ref: Option<String>,
}

pub type Output = ProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
