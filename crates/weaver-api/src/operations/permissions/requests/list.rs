use super::prelude::*;

/// List durable external-access requests for this session.
#[operation(
    id = "permissions.requests.list",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/permissions/read@v1"],
    cli = "permissions requests",
    mcp = "loom_permission::requests",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Restrict to `pending`, `approved`, or `denied`. Omit to list all.
    pub state: Option<String>,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = Vec<PermissionRequestView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
