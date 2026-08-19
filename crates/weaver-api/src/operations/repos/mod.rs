//! The managed repository store, the clone allowlist, and per-repo
//! environment variables.
//!
//! Every operation here answers to a human operator addressing an arbitrary
//! repository (by slug, or by a server-local `cwd` path), not a session
//! credential addressing its own — so the whole bundle is `actor = User`, no
//! `mcp =`. `user_grant_allows` in `crates/loom/src/web/auth.rs` permits any
//! signed-in operator (not just `Admin`) to reach every mutating `/repos*`
//! route (registering a repo, setting repo env), unlike the `/agents/custom*`
//! or `/watches*` families it explicitly excludes — so this stays `User`
//! rather than `Admin`. The three read-only escapes (`recent`, `branches`,
//! `revisions/validate`) exist only for the interactive "new session" launch
//! flow (`NewSessionDrawer.vue`), not for an agent driving itself, and a
//! session credential has never been able to reach two of the three
//! (`revisions/validate` and `env` are absent from `grant_allows`'s
//! session-reachable list) — so they stay `User` too, for one consistent
//! bundle-wide policy rather than a per-route split.
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
