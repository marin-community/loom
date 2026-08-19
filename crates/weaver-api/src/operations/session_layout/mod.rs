//! The signed-in operator's shared session-dashboard layout: spaces, groups,
//! and the placement of sessions within them.
//!
//! This is dashboard state, not a session credential's surface — every
//! mutation is keyed off the calling human's own username and every write
//! carries an `expected_revision` optimistic-concurrency guard, since more
//! than one open dashboard tab can race to reorganize the same layout.
//!
//! One file per operation. Adding `session_layout.spaces.update` means adding
//! its file here and its handler in the mirrored server tree — no clap
//! variant, no client wrapper, no MCP schema, no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod get;
pub mod groups;
pub mod r#move;
pub mod reorder;
pub mod restore;
pub mod spaces;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <spaces::create::Create as Operation>::SPEC,
    <groups::create::Create as Operation>::SPEC,
    <r#move::Move as Operation>::SPEC,
    <reorder::Reorder as Operation>::SPEC,
    <restore::Restore as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        // The `#[operation(...)]` macro derives an operation's `bundle` field
        // from its id's first dotted segment with `_` replaced by `-` (see
        // `loom-api-macros/src/operation.rs`), so this must match that, not
        // the `session_layout` directory/module name the id itself uses.
        name: "session-layout",
        label: "Session layout",
        operations: OPERATIONS,
    }
}
