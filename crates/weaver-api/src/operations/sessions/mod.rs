//! Session lifecycle, status projection, and normalized history.
//!
//! One file per operation, mirroring the `issues` bundle. `self.get` keeps its
//! historical id — a caller-facing bootstrap read — but lives in this bundle;
//! its module is named `self_context` rather than `self` because `self` cannot
//! be a module name, and it carries an explicit `bundle = "sessions"` so the
//! id's own `self` prefix does not get inferred as its bundle.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod events;
pub mod get;
pub mod history;
pub mod interrupt;
pub mod launch;
pub mod list;
pub mod preview;
pub mod self_context;
pub mod send;
pub mod status;
pub mod summary;
pub mod tags;

static OPERATIONS: &[&OperationSpec] = &[
    <self_context::Get as Operation>::SPEC,
    <summary::get::Get as Operation>::SPEC,
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <launch::Launch as Operation>::SPEC,
    <send::Send as Operation>::SPEC,
    <interrupt::Interrupt as Operation>::SPEC,
    <preview::Preview as Operation>::SPEC,
    <events::list::List as Operation>::SPEC,
    <events::create::Create as Operation>::SPEC,
    <history::list::List as Operation>::SPEC,
    <history::search::Search as Operation>::SPEC,
    <status::get::Get as Operation>::SPEC,
    <status::set::Set as Operation>::SPEC,
    <tags::list::List as Operation>::SPEC,
    <tags::set::Set as Operation>::SPEC,
    <tags::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "sessions",
        label: "Session workflow",
        operations: OPERATIONS,
    }
}
