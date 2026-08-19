use super::prelude::*;

/// Remove a workload-identity federation mapping.
///
/// Operator-only, same reasoning as [`create`](super::create).
#[operation(
    id = "auth.federations.remove",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "federation rm",
)]
pub struct Remove;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The mapping id (from `federation ls`).
    #[operand(positional)]
    pub id: String,
}

pub type Output = RemoveFederationResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
