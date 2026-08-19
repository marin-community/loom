//! Provider-neutral inspection and administration of Loom's MCP registry:
//! built-in adapters, versioned capability sets, and operator-authored
//! custom servers.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod custom;
pub mod get;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <custom::list::List as Operation>::SPEC,
    <custom::get::Get as Operation>::SPEC,
    <custom::create::Create as Operation>::SPEC,
    <custom::update::Update as Operation>::SPEC,
    <custom::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "mcps",
        label: "MCP registry",
        operations: OPERATIONS,
    }
}
