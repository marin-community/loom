//! Watches — periodic / triggered watch programs over the fleet.
//!
//! The operator + authoring surface over `weaver_core::watch`: register a
//! watch, arm/disarm it, fire a round on demand, and inspect its round
//! history. Previously excluded from the registry entirely as "fleet
//! automation" — every route here already required a human `User` or `Admin`
//! credential (see `crates/loom/src/web/auth.rs`'s `user_grant_allows`, which
//! refuses a plain `User` grant on every mutating `/watches` route); the
//! registry now says so as a field instead of by omission.
//!
//! One file per operation. Adding `watches.clone` means adding its file here
//! and its handler in the mirrored server tree — no clap variant, no client
//! wrapper, no MCP schema, no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod create;
pub mod delete;
pub mod get;
pub mod list;
pub mod programs;
pub mod run;
pub mod runs;
pub mod update;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <programs::Programs as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <run::Run as Operation>::SPEC,
    <runs::Runs as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "watches",
        label: "Watches",
        operations: OPERATIONS,
    }
}
