//! Branches — the per-worktree unit of work a session is attached to.
//!
//! Most operations are `actor = SessionSelf` scoped to the caller's own branch.
//! `branches.list` is fleet-wide (`scope = Global`).

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod events {
    //! Durable branch lifecycle events.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Append a raw event row to a branch's log — the escape hatch for an event
        /// kind with no dedicated mutating route of its own.
        ///
        /// The branch-scoped twin of `sessions.events.create`; `branches.events.list`
        /// reads the same log this appends to.
        #[operation(id = "branches.events.create", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/branches/write@v1"],
                    cli = "branches events create")]
        pub struct Input {
            /// The event kind, e.g. an agent hook name.
            pub kind: String,
            /// Arbitrary event payload.
            #[operand(json, default = serde_json::Value::Null)]
            pub data: serde_json::Value,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = weaver_core::events::Event;
    }

    pub mod list {
        use super::prelude::*;

        /// List recent durable events on a branch (newest first, last 200 entries).
        #[operation(id = "branches.events.list", actor = SessionSelf, scope = Branch, risk = Read,
                    grants = ["loom/branches/read@v1"], cli = "branches events list")]
        pub struct Input {
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<weaver_core::events::Event>;
    }
}

pub mod get {
    use super::prelude::*;

    /// Inspect one branch.
    #[operation(id = "branches.get", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/branches/read@v1"], cli = "branches get")]
    pub struct Input {
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = BranchView;
}

pub mod issues {
    //! Work items claimed by a branch.
    pub(super) use super::prelude;
    pub mod list {
        use super::prelude::*;

        /// List work items claimed by this branch — the session's working set.
        ///
        /// Branch-scoped, unlike `issues.list`, which is keyed by `repo_root`.
        #[operation(id = "branches.issues.list", actor = SessionSelf, scope = Branch, risk = Read,
                    grants = ["loom/branches/read@v1"], cli = "branches issues list")]
        pub struct Input {
            /// Include closed work items.
            #[operand(default = false)]
            pub all: bool,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<IssueView>;
    }
}

pub mod list {
    use super::prelude::*;

    /// List every branch loom is tracking (fleet-wide, unfiltered).
    #[operation(id = "branches.list", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/branches/read@v1"], cli = "branches list")]
    pub struct Input {}

    pub type Output = Vec<BranchView>;
}

pub mod slack {
    //! Posting back to Slack on behalf of a branch's session.
    pub(super) use super::prelude;
    pub mod reply {
        use super::prelude::*;

        /// Post a message from this branch's session back to a Slack thread.
        ///
        /// Without `thread`, replies to the branch's own Slack wiring; with `thread`,
        /// targets a delivered thread.
        #[operation(id = "branches.slack.reply", actor = SessionSelf, scope = Branch,
                    risk = ExternalWrite, grants = ["loom/branches/write@v1"],
                    cli = "branches slack reply", mcp = "loom_messaging::slack_reply")]
        pub struct Input {
            /// The message text.
            #[operand(positional)]
            pub text: String,
            /// Delivered thread to reply in (optional).
            #[operand(json, default = None)]
            pub thread: Option<SlackThreadRef>,
            /// Dedupe key so a retried send doesn't double-post.
            pub idempotency_key: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = serde_json::Value;
    }
}

pub mod status {
    //! The branch's durable attention level and status message.
    pub(super) use super::prelude;
    pub mod set {
        use super::prelude::*;

        /// Set the branch's attention level and current-state message in one call.
        ///
        /// The branch-scoped twin of `sessions.status.set`, for a target with no live
        /// session bound to it (a finished session, or an id naming another branch
        /// entirely).
        #[operation(id = "branches.status.set", actor = SessionSelf, scope = Branch, risk = Write,
                    grants = ["loom/branches/write@v1"], cli = "branches status set")]
        pub struct Input {
            /// The attention level: `ok`, `attention`, or `blocked`.
            #[operand(long = "tag")]
            pub level: String,
            /// The current-state message shown alongside the level. Absent/empty
            /// leaves the previous message in place.
            pub message: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = BranchView;
    }
}

pub mod tags {
    //! Free-form `(key, value)` annotations on a branch.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one free-form tag from a branch — the branch-scoped twin of
        /// `sessions.tags.delete`.
        #[operation(id = "branches.tags.delete", actor = SessionSelf, scope = Branch, risk = Write,
                    grants = ["loom/branches/write@v1"], cli = "branches tags delete")]
        pub struct Input {
            /// The tag key to remove.
            #[operand(positional)]
            pub key: String,
            /// Who is clearing it (a watch name, or blank for `manual`).
            pub by: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = BranchView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set one free-form tag on a branch — the branch-scoped twin of
        /// `sessions.tags.set`, for a target with no live session bound to it (a
        /// finished session, or an id naming another branch entirely).
        #[operation(id = "branches.tags.set", actor = SessionSelf, scope = Branch, risk = Write,
                    grants = ["loom/branches/write@v1"], cli = "branches tags set")]
        pub struct Input {
            /// The tag key.
            #[operand(positional)]
            pub key: String,
            /// The tag value.
            #[operand(positional)]
            pub value: String,
            /// One-line reason accompanying the tag.
            #[operand(default = String::new())]
            pub note: String,
            /// Who is setting it (a watch name, or blank for `manual`).
            pub by: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = BranchView;
    }
}

pub mod update {
    use super::prelude::*;

    /// Update a branch's title, goal, or current-state description.
    ///
    /// Title updates require `expected_title` and `expected_title_provenance`
    /// to detect and reject concurrent renames.
    #[operation(id = "branches.update", actor = SessionSelf, scope = Branch, risk = Write,
                grants = ["loom/branches/write@v1"], cli = "branches update")]
    pub struct Input {
        pub title: Option<String>,
        /// Required with `title`.
        pub expected_title: Option<String>,
        /// Required with `title`.
        pub expected_title_provenance: Option<String>,
        pub goal: Option<String>,
        /// The agent's current-state message.
        pub description: Option<String>,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = BranchView;
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    get::SPEC,
    update::SPEC,
    status::set::SPEC,
    slack::reply::SPEC,
    events::list::SPEC,
    events::create::SPEC,
    tags::set::SPEC,
    tags::delete::SPEC,
    issues::list::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "branches",
        label: "Branches",
        operations: OPERATIONS,
    }
}
