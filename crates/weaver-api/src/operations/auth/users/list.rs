use super::prelude::*;

/// List the approved operators.
///
/// Previously excluded from the registry as "administrative". Operator
/// management was never reachable by a plain `User` grant
/// (`user_grant_allows` refuses every `/auth/users` route in
/// `crates/loom/src/web/auth.rs`) — the registry now says so with
/// `actor = Admin` instead of by leaving the route out entirely.
#[operation(
    id = "auth.users.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "auth users ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<UserView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
