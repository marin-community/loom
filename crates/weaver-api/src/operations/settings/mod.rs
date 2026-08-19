//! Server-wide runtime settings and the default profile's environment
//! compatibility facade.
//!
//! `loom config render-env`, `secret-names`, `push-secrets`, and `set` are
//! deliberately absent: they read/write `loom.toml` or the sqlite `settings`
//! table directly with no running server, so they are not operations. The
//! REST surface here — `GET`/`PATCH /api/settings` and the `/api/env`
//! facade it exposes to operators — is.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod env;
pub mod get;
pub mod patch;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <patch::Patch as Operation>::SPEC,
    <env::list::List as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "settings",
        label: "Runtime settings",
        operations: OPERATIONS,
    }
}
