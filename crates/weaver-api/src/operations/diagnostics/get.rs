use super::prelude::*;

/// The aggregated fleet diagnostics snapshot: session/profile capacity,
/// automation run health, migration state, and federation mappings.
///
/// `actor = User`: the legacy `GET /diagnostics` handler refused a non-human
/// principal outright (`if !principal.is_human() { return Err(FORBIDDEN) }`);
/// `actor = User` expresses that structurally instead of as an inline check.
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
