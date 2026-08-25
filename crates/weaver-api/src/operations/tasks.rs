//! Detached background tasks — the GitHub `@loom` trigger launches that run
//! off the webhook request, surfaced on the Debug page so an operator can
//! watch one being handled after its `200` was already returned. Backed by
//! `loom_watch::tasks`'s in-memory ring buffer: an observability aid, not a
//! job queue.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod list {
    use super::prelude::*;

    /// List recent detached background tasks — currently the GitHub `@loom`
    /// trigger launches, which run off the webhook request so a slow clone can't
    /// blow GitHub's delivery timeout — newest first.
    #[operation(id = "tasks.list", actor = User, scope = Global, risk = Read, cli = "tasks list")]
    pub struct Input {}

    pub type Output = Vec<TaskView>;
}

static OPERATIONS: &[&OperationSpec] = &[list::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "tasks",
        label: "Background tasks",
        operations: OPERATIONS,
    }
}
