use super::prelude::*;

/// Restart the standalone operator shell, discarding its process state.
#[operation(
    id = "shell.restart",
    actor = Admin,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
    cli = "shell restart",
)]
pub struct Restart;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = ShellRestartResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
