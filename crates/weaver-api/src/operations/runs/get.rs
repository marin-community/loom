use super::prelude::*;

/// Inspect one automation-triggered run by id.
///
/// `actor = User`, matching `runs.list`.
#[operation(
    id = "runs.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "runs get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The run id.
    #[operand(positional)]
    pub id: String,
}

pub type Output = RunView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
