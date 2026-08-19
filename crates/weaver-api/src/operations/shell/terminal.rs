use super::prelude::*;

/// Attach to the standalone operator shell over a websocket.
#[operation(
    id = "shell.terminal",
    actor = Admin,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
    io = Duplex,
)]
pub struct Terminal;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
