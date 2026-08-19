//! Repository work items.
//!
//! One file per operation. Adding `issues.archive` means adding `archive.rs`
//! here and its handler in the mirrored server tree — no clap variant, no client
//! wrapper, no MCP schema, no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod actions;
pub mod backlog;
pub mod close;
pub mod create;
pub mod delete;
pub mod get;
pub mod list;
pub mod reopen;
pub mod tags;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <backlog::create::Create as Operation>::SPEC,
    <close::Close as Operation>::SPEC,
    <reopen::Reopen as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <tags::set::Set as Operation>::SPEC,
    <tags::delete::Delete as Operation>::SPEC,
    <actions::Actions as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "issues",
        label: "Work items",
        operations: OPERATIONS,
    }
}
