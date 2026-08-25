//! Repository work items.
//!
//! Adding `issues.archive` means adding it here and its handler in
//! `crates/loom/src/web/issues.rs` — the registry derives its clap variant,
//! client wrapper, MCP schema, and capability set.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod actions {
    //! The general bulk form: one action applied atomically to a set of work items.
    //!
    //! `IssueAction` is an internally-tagged union. Deriving the schema from the
    //! real type prevents invalid field combinations at compile time, making this
    //! operation register like any other in the registry.

    use super::prelude::*;

    /// Apply one action atomically to a set of work items.
    #[operation(id = "issues.actions", actor = SessionSelf, scope = Repository, risk = Write,
                grants = ["loom/issues/write@v1"], cli = "issues actions",
                mcp = "loom_issue::actions", render = custom, default = custom)]
    pub struct Input {
        /// The work items to act on. Either every id succeeds or none does.
        #[operand(long = "id")]
        pub ids: Vec<i64>,
        /// The action to apply — `close`, `reopen`, `delete`, `tag`, or `untag`.
        /// On the command line this takes a JSON object, because a tagged union is
        /// not a flag.
        #[operand(json)]
        pub action: IssueAction,
        #[operand(context)]
        pub repo_root: String,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                ids: Vec::new(),
                action: IssueAction::Close,
                repo_root: String::new(),
            }
        }
    }

    pub type Output = IssueActionsResult;
}

pub mod backlog {
    //! Unclaimed repository backlog items.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Create an unclaimed repository backlog item.
        #[operation(id = "issues.backlog.create", actor = SessionSelf, scope = Repository,
                    risk = Write, grants = ["loom/issues/write@v1"], cli = "issues backlog add",
                    mcp = "loom_issue::backlog_add", render = custom)]
        pub struct Input {
            /// One-line summary of the work.
            #[operand(positional)]
            pub title: String,
            /// Optional detail.
            #[operand(default = String::new())]
            pub body: String,
            /// Link the item to an existing GitHub issue number.
            pub github_issue: Option<i64>,
            /// Tags to apply in the same transaction as the insert.
            ///
            /// Atomic on purpose: the create-issue form stages tags before the item
            /// exists, and applying them afterwards would leave a window where the board
            /// shows an untagged item — or, if the second call fails, keeps it untagged.
            #[operand(json, default = Vec::new())]
            pub tags: Vec<IssueTagInput>,
            #[operand(context)]
            pub repo_root: String,
            /// The branch that filed this item, for provenance.
            ///
            /// The branch *name*, not its id — this is compared against `branch.branch`
            /// when the CLI decides whether an item was delegated by the current branch.
            #[operand(context = "branch_name")]
            pub source_branch: Option<String>,
        }

        pub type Output = IssueView;
    }
}

pub mod board {
    use super::prelude::*;

    /// Every work item across every repository — the dashboard's board.
    ///
    /// Separate from `issues.list` rather than one operation with an optional
    /// parameter, because a scope that changes with input cannot be authorized.
    #[operation(id = "issues.board", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/issues/read@v1"], cli = "issues board")]
    pub struct Input {
        /// Include closed work items.
        #[operand(default = false)]
        pub all: bool,
        /// Include items claimed by an automation-class session's branch. Defaults
        /// to `false` — the board shows the work of the interactive fleet, not the
        /// trackers its machinery opens for itself.
        #[operand(default = false)]
        pub automation: bool,
    }

    pub type Output = Vec<IssueView>;
}

pub mod close {
    use super::prelude::*;

    /// Close one or more work items atomically.
    #[operation(id = "issues.close", actor = SessionSelf, scope = Repository, risk = Write,
                grants = ["loom/issues/write@v1"], cli = "issues close", mcp = "loom_issue::close",
                render = custom)]
    pub struct Input {
        /// One or more Loom work-item ids. Applied atomically: either every id
        /// succeeds or none does.
        #[operand(positional)]
        pub ids: Vec<i64>,
        #[operand(context)]
        pub repo_root: String,
    }

    pub type Output = IssueActionsResult;
}

pub mod create {
    use super::prelude::*;

    /// Create a work item claimed by this session's branch.
    #[operation(id = "issues.create", actor = SessionSelf, scope = Branch, risk = Write,
                grants = ["loom/issues/write@v1"], cli = "issues add", mcp = "loom_issue::add",
                render = custom)]
    pub struct Input {
        /// One-line summary of the work.
        #[operand(positional)]
        pub title: String,
        /// Optional detail.
        #[operand(default = String::new())]
        pub body: String,
        /// Link the item to an existing GitHub issue number.
        pub github_issue: Option<i64>,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = IssueView;
}

pub mod delete {
    use super::prelude::*;

    /// Permanently delete one or more work items atomically.
    #[operation(id = "issues.delete", actor = SessionSelf, scope = Repository, risk = Destructive,
                grants = ["loom/issues/write@v1"], cli = "issues delete", cli_alias = "rm",
                mcp = "loom_issue::delete", render = custom)]
    pub struct Input {
        /// One or more Loom work-item ids. Applied atomically: either every id
        /// succeeds or none does.
        #[operand(positional)]
        pub ids: Vec<i64>,
        #[operand(context)]
        pub repo_root: String,
    }

    pub type Output = IssueActionsResult;
}

pub mod get {
    use super::prelude::*;

    /// Inspect one work item and the status of the branch working it.
    #[operation(id = "issues.get", actor = SessionSelf, scope = Repository, risk = Read,
                grants = ["loom/issues/read@v1"], cli = "issues get", cli_alias = "show",
                mcp = "loom_issue::get", render = custom)]
    pub struct Input {
        /// A Loom work-item id.
        #[operand(positional)]
        pub id: i64,
        #[operand(context)]
        pub repo_root: String,
    }

    pub type Output = IssueView;
}

pub mod list {
    //! `issues.list` — the reference shape for every operation in the registry.
    //!
    //! Read top to bottom, this is the whole contract: who may call it, what it
    //! accepts, what it returns, and how it prints. REST, the CLI, and MCP are all
    //! generated from what is here; none of them adds arguments of its own.

    use super::prelude::*;

    /// List current-session and repository work items.
    #[operation(id = "issues.list", actor = SessionSelf, scope = Repository, risk = Read,
                grants = ["loom/issues/read@v1"], cli = "issues list", cli_alias = "ls",
                mcp = "loom_issue::list", view = View, render = custom)]
    pub struct Input {
        #[operand(context)]
        pub repo_root: String,
        /// Include closed work items.
        #[operand(default = false)]
        pub all: bool,
        /// List only unclaimed backlog items — those no branch has picked up.
        #[operand(default = false)]
        pub backlog: bool,
    }

    pub type Output = Vec<IssueView>;

    /// Presentation flags. These never cross the wire — they choose how the result
    /// is printed, which is why they live here rather than in `Input`.
    #[derive(Debug, Clone, Default, View)]
    pub struct View {
        /// Show every work item in the repository, uncapped.
        pub repo: bool,
        /// Show only the items claimed by this branch.
        pub mine: bool,
    }
}

pub mod reopen {
    use super::prelude::*;

    /// Reopen one or more closed work items atomically.
    #[operation(id = "issues.reopen", actor = SessionSelf, scope = Repository, risk = Write,
                grants = ["loom/issues/write@v1"], cli = "issues reopen",
                mcp = "loom_issue::reopen", render = custom)]
    pub struct Input {
        /// One or more Loom work-item ids. Applied atomically: either every id
        /// succeeds or none does.
        #[operand(positional)]
        pub ids: Vec<i64>,
        #[operand(context)]
        pub repo_root: String,
    }

    pub type Output = IssueActionsResult;
}

pub mod tags {
    //! Free-form `(key, value)` annotations on work items.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one free-form tag from a work item.
        #[operation(id = "issues.tags.delete", actor = SessionSelf, scope = Repository,
                    risk = Write, grants = ["loom/issues/write@v1"], cli = "issues tag delete",
                    cli_alias = "rm", mcp = "loom_issue::tag_delete", render = custom)]
        pub struct Input {
            /// A Loom work-item id.
            #[operand(positional)]
            pub id: i64,
            /// The tag key to remove.
            #[operand(positional)]
            pub key: String,
            #[operand(context)]
            pub repo_root: String,
        }

        pub type Output = IssueView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set one free-form tag on a work item.
        #[operation(id = "issues.tags.set", actor = SessionSelf, scope = Repository, risk = Write,
                    grants = ["loom/issues/write@v1"], cli = "issues tag set",
                    mcp = "loom_issue::tag_set", render = custom)]
        pub struct Input {
            /// A Loom work-item id.
            #[operand(positional)]
            pub id: i64,
            /// The tag key.
            #[operand(positional)]
            pub key: String,
            /// The tag value. Use `issues tag delete` to clear a tag.
            #[operand(positional)]
            pub value: String,
            /// One-line reason accompanying the tag.
            #[operand(default = String::new())]
            pub note: String,
            #[operand(context)]
            pub repo_root: String,
        }

        pub type Output = IssueView;
    }
}

pub mod update {
    use super::prelude::*;

    /// Edit a work item's own fields.
    ///
    /// Claiming is not here: a claim is made by launching a session against an
    /// item, so the only claim change this expresses is `unclaim: bool`, which
    /// returns the item to the backlog and cannot represent any other transition.
    #[operation(id = "issues.update", actor = SessionSelf, scope = Repository, risk = Write,
                grants = ["loom/issues/write@v1"], cli = "issues update")]
    pub struct Input {
        /// A Loom work-item id.
        #[operand(positional)]
        pub id: i64,
        /// Replace the one-line summary.
        pub title: Option<String>,
        /// Replace the detail body.
        pub body: Option<String>,
        /// `open` or `closed`.
        pub status: Option<String>,
        /// GitHub issue mapping as `owner/name#number`. An empty string clears the
        /// mapping; omitting the field leaves it unchanged.
        pub github: Option<String>,
        /// Return the item to the unclaimed backlog.
        #[operand(default = false)]
        pub unclaim: bool,
        #[operand(context)]
        pub repo_root: String,
    }

    pub type Output = IssueView;
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    board::SPEC,
    get::SPEC,
    create::SPEC,
    update::SPEC,
    backlog::create::SPEC,
    close::SPEC,
    reopen::SPEC,
    delete::SPEC,
    tags::set::SPEC,
    tags::delete::SPEC,
    actions::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "issues",
        label: "Work items",
        operations: OPERATIONS,
    }
}
