//! The standalone operator shell.
//!
//! A terminal on the loom host itself (not attached to any session), requiring
//! `actor = Admin`.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod restart {
    use super::prelude::*;

    /// Restart the standalone operator shell, discarding its process state.
    #[operation(id = "shell.restart", actor = Admin, scope = Global, risk = ExternalWrite,
                cli = "shell restart")]
    pub struct Input {}

    pub type Output = ShellRestartResult;
}

pub mod terminal {
    use super::prelude::*;

    /// Attach to the standalone operator shell over a websocket.
    #[operation(id = "shell.terminal", actor = Admin, scope = Global, risk = ExternalWrite,
                io = Duplex)]
    pub struct Input {}

    pub type Output = ();
}

static OPERATIONS: &[&OperationSpec] = &[restart::SPEC, terminal::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "shell",
        label: "Operator shell",
        operations: OPERATIONS,
    }
}
