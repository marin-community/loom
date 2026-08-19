//! Durable conversation channels.
//!
//! Session channels, custom channels, their messages, subscriptions, and read
//! markers. One file per operation. Adding `channels.archive` means adding
//! its file here and its handler in the mirrored server tree — no clap
//! variant, no client wrapper, no MCP schema, no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod create;
pub mod get;
pub mod list;
pub mod messages;
pub mod read_marker;
pub mod subscription;
pub mod wait;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <messages::list::List as Operation>::SPEC,
    <messages::create::Create as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <subscription::set::Set as Operation>::SPEC,
    <read_marker::set::Set as Operation>::SPEC,
    <wait::Wait as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "channels",
        label: "Durable conversations",
        operations: OPERATIONS,
    }
}
