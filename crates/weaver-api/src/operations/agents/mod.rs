//! Agent runtimes: the picker list (builtins + operator-defined custom
//! agents) and the custom-agent editor's CRUD.
//!
//! `GET /api/agents` is a plain fleet-wide read a session credential has
//! always been able to reach — `grant_allows` in
//! `crates/loom/src/web/auth.rs` lists bare `/agents` beside `/sessions`,
//! `/branches`, and `/issues` — so it stays `actor = SessionSelf`. Defining,
//! editing, and removing a custom agent are different in kind:
//! `user_grant_allows` explicitly refuses a bare `User` grant on every
//! mutating `/agents/custom*` route, so only `Admin` may reach them — this is
//! fleet configuration (which runtimes exist at all), not a per-branch action
//! a signed-in user takes on their own behalf, exactly like `watches.create`.
//!
//! One file per operation, mirroring `issues`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod custom;
pub mod list;
pub mod oneshot;

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <custom::create::Create as Operation>::SPEC,
    <custom::update::Update as Operation>::SPEC,
    <custom::delete::Delete as Operation>::SPEC,
    <oneshot::Oneshot as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "agents",
        label: "Agent runtimes",
        operations: OPERATIONS,
    }
}
