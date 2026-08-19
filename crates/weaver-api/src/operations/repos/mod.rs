//! The managed repository store, the clone allowlist, and per-repo
//! environment variables.
//!
//! One file per operation, mirroring `issues`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod branches;
pub mod env;
pub mod list;
pub mod recent;
pub mod register;
pub mod revisions;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <register::Register as Operation>::SPEC,
    <recent::Recent as Operation>::SPEC,
    <branches::List as Operation>::SPEC,
    <revisions::validate::Validate as Operation>::SPEC,
    <env::get::Get as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "repos",
        label: "Managed repositories",
        operations: OPERATIONS,
    }
}
