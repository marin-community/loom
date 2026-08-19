//! Branches — the per-worktree unit of work a session is attached to.
//!
//! `GET /api/branches` and its siblings were previously unregistered
//! entirely. A branch is what a session (and its `LOOM_TOKEN` credential) is
//! bound to, so almost everything here is `actor = SessionSelf` scoped to
//! exactly the caller's own branch — see `require_branch_access` in
//! `crates/loom/src/web/scope.rs`, which lets a session credential reach only
//! the branch it is bound to (by id or name) while a `User`/`Admin` principal
//! may address any branch. `branches.list` is the one exception: it is a
//! fleet-wide, unfiltered read a session credential has always been able to
//! reach (`grant_allows` in `crates/loom/src/web/auth.rs` lists bare
//! `/branches` beside `/sessions` and `/issues`), mirroring `sessions.list`'s
//! own `scope = Global`.
//!
//! One file per operation, mirroring `issues`. Adding `branches.archive`
//! means adding `archive.rs` here and its handler in the mirrored server tree
//! — no clap variant, no client wrapper, no MCP schema, no capability set.

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
