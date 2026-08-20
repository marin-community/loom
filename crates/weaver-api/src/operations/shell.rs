//! The standalone operator shell.
//!
//! A terminal on the loom host itself (not attached to any session), requiring
//! `actor = Admin`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod restart {
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
}

pub mod terminal {
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
}

static OPERATIONS: &[&OperationSpec] = &[
    <restart::Restart as Operation>::SPEC,
    <terminal::Terminal as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "shell",
        label: "Operator shell",
        operations: OPERATIONS,
    }
}
