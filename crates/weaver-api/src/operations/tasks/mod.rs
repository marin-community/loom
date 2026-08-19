//! Detached background tasks — the GitHub `@loom` trigger launches that run
//! off the webhook request, surfaced on the Debug page so an operator can
//! watch one being handled after its `200` was already returned. Backed by
//! `loom_watch::tasks`'s in-memory ring buffer: an observability aid, not a
//! job queue.
//!
//! Previously excluded from the registry as "fleet automation"; it was always
//! human-only diagnostics (`crates/loom/src/web/logview.rs`), so it registers
//! as `actor = User`, not omitted.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod list;

static OPERATIONS: &[&OperationSpec] = &[<list::List as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "tasks",
        label: "Background tasks",
        operations: OPERATIONS,
    }
}
