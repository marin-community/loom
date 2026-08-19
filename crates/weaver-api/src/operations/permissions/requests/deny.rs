//! Previously excluded from the registry as "human-only". See `approve` for
//! why `actor = User` (a field) replaces omission as the mechanism.

use super::prelude::*;

/// Deny a pending external-access request.
#[operation(
    id = "permissions.requests.deny",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "permissions deny",
)]
pub struct Deny;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The pending permission request id.
    #[operand(positional)]
    pub request: String,
    /// Optional audit reason recorded with the decision.
    #[operand(default = String::new())]
    pub reason: String,
}

pub type Output = PermissionRequestView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
