use super::prelude::*;

/// The aggregated fleet diagnostics snapshot: session/profile capacity,
/// automation run health, migration state, and federation mappings.
#[operation(
    id = "diagnostics.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = DiagnosticsView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
