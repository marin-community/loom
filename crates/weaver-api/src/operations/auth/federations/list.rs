use super::prelude::*;

/// List the registered workload-identity federation mappings.
///
/// Operator-only, same reasoning as [`create`](super::create):
/// `user_grant_allows` refuses a plain `User` grant on every
/// `/auth/federations` route.
#[operation(
    id = "auth.federations.list",
    actor = Admin,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "federation ls",
)]
pub struct List;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<FederationView>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
