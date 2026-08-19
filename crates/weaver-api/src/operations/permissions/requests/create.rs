use super::prelude::*;

/// Request a human-approved GitHub write-access expansion for this session.
#[operation(
    id = "permissions.requests.create",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/permissions/request@v1"],
    cli = "permissions request github-repository",
    mcp = "loom_permission::request",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The `owner/repo` slug to request write access to.
    #[operand(positional)]
    pub repository: String,
    /// Why the task needs this repository.
    pub reason: String,
    /// Currently only `write` is accepted.
    #[operand(default = "write")]
    pub mode: String,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub session: String,
}

pub type Output = PermissionRequestView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
