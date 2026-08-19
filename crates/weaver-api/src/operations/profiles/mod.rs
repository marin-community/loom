//! Named session-launch profiles and their write-only environment.
//!
//! A profile is a reusable, named launch template — the agent runtime,
//! model, MCP policy, and other launch-time policy a session inherits by
//! name. Secret environment values are write-only: every read-side view
//! carries metadata only, never a stored value.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod clone;
pub mod create;
pub mod delete;
pub mod effective;
pub mod env;
pub mod get;
pub mod list;
pub mod update;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <effective::Effective as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <clone::Clone as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "profiles",
        label: "Launch profiles",
        operations: OPERATIONS,
    }
}
