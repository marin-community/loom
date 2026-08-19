use super::prelude::*;

/// Inspect one watch by id or name.
#[operation(
    id = "watches.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Watch id or name.
    #[operand(positional)]
    pub key: String,
}

pub type Output = WatchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
