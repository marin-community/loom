use super::prelude::*;

/// Tail the server log as it is written.
///
/// `actor = User`: human-only self-service debugging, the same policy the
/// snapshot endpoints carry. A session credential has never been able to reach
/// the log routes.
#[operation(
    id = "logs.stream",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    io = Stream,
)]
pub struct Stream;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
