use super::prelude::*;

/// Show this session's effective Loom operations and external repository
/// scope.
#[operation(
    id = "permissions.effective.get",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions show",
    mcp = "loom_permission::show",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = EffectivePermissionsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
