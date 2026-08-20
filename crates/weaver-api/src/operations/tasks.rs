//! Detached background tasks — the GitHub `@loom` trigger launches that run
//! off the webhook request, surfaced on the Debug page so an operator can
//! watch one being handled after its `200` was already returned. Backed by
//! `loom_watch::tasks`'s in-memory ring buffer: an observability aid, not a
//! job queue.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod list {
    use super::prelude::*;

    /// List recent detached background tasks — currently the GitHub `@loom`
    /// trigger launches, which run off the webhook request so a slow clone can't
    /// blow GitHub's delivery timeout — newest first.
    ///
    /// `actor = User`: human-only self-service debugging (Settings →
    /// Diagnostics), same as the log endpoints. No session grant can reach
    /// `/tasks`, so this is `User` rather than `SessionSelf`.
    #[operation(
    id = "tasks.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "tasks list",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<TaskView>;
}

static OPERATIONS: &[&OperationSpec] = &[<list::List as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "tasks",
        label: "Background tasks",
        operations: OPERATIONS,
    }
}
