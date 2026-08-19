use super::prelude::*;

/// Remove a watch.
///
/// Operator-only, same reasoning as `watches.create`.
#[operation(
    id = "watches.delete",
    actor = Admin,
    scope = Global,
    risk = Destructive,
    grants = [],
    cli = "watch rm",
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Watch id or name.
    #[operand(positional)]
    pub key: String,
}

pub type Output = WatchDeleteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
