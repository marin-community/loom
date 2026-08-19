//! Previously excluded from the registry as "human-only". A human deciding a
//! pending request is exactly what `actor = User` means: registered, callable
//! over REST and the CLI, and — because a non-`SessionSelf` operation cannot
//! carry an `mcp` projection — structurally unreachable by an agent.

use super::prelude::*;

/// Approve and apply a pending external-access request.
#[operation(
    id = "permissions.requests.approve",
    actor = User,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
    cli = "permissions approve",
)]
pub struct Approve;

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
