//! Branches — the per-worktree unit of work a session is attached to.
//!
//! Most operations are `actor = SessionSelf` scoped to the caller's own branch.
//! `branches.list` is fleet-wide (`scope = Global`).
//!
//! One file per operation. Adding a new operation means adding a file here
//! and its handler in the corresponding server tree.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod events;
pub mod get;
pub mod issues;
pub mod list;
pub mod slack;
pub mod status;
pub mod tags;
pub mod update;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <status::set::Set as Operation>::SPEC,
    <slack::reply::Reply as Operation>::SPEC,
    <events::list::List as Operation>::SPEC,
    <events::create::Create as Operation>::SPEC,
    <tags::set::Set as Operation>::SPEC,
    <tags::delete::Delete as Operation>::SPEC,
    <issues::list::List as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "branches",
        label: "Branches",
        operations: OPERATIONS,
    }
}
