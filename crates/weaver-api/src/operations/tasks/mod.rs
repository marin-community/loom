//! Detached background tasks — the GitHub `@loom` trigger launches that run
//! off the webhook request, surfaced on the Debug page so an operator can
//! watch one being handled after its `200` was already returned. Backed by
//! `loom_watch::tasks`'s in-memory ring buffer: an observability aid, not a
//! job queue.

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
