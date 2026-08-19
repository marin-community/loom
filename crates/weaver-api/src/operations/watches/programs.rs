use super::prelude::*;

/// List the builtin watch programs that ship with loom — what the create form
/// offers and the panel's read-only script viewer renders.
#[operation(
    id = "watches.programs",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "watch programs",
    view = View,
)]
pub struct Programs;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = Vec<ProgramView>;

/// CLI-only flags that never cross the wire: the full registry is always
/// fetched, this only chooses what gets printed.
#[derive(Debug, Clone, Default, View)]
pub struct View {
    /// Print one program's embedded script source instead of the table, e.g.
    /// `--source builtin:archive-merged`.
    pub source: Option<String>,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
