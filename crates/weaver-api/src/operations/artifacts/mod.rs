//! Versioned deliverables.
//!
//! Named documents an agent (or the user) writes to weaver — plus the
//! anchored review threads discussed against them. One file per operation.
//! Adding `artifacts.share` means adding `share.rs` here and its handler in
//! the mirrored server tree — no clap variant, no client wrapper, no MCP
//! schema, no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod delete;
pub mod get;
pub mod history;
pub mod list;
pub mod raw;
pub mod threads;
pub mod url;
pub mod write;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <raw::Raw as Operation>::SPEC,
    <write::Write as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <history::History as Operation>::SPEC,
    <url::Url as Operation>::SPEC,
    <threads::list::List as Operation>::SPEC,
    <threads::comment::Comment as Operation>::SPEC,
    <threads::resolve::Resolve as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "artifacts",
        label: "Versioned deliverables",
        operations: OPERATIONS,
    }
}
