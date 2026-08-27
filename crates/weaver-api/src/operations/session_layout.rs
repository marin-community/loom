//! The signed-in operator's shared session-dashboard layout: spaces, groups,
//! and the placement of sessions within them.
//!
//! This is dashboard state, not something a session credential can reach — every
//! mutation is keyed off the calling human's own username and every write may
//! carry an `expected_revision` optimistic-concurrency guard, since more than
//! one open dashboard tab can race to reorganize the same layout. Omitting the
//! guard applies the change to whatever is current; `loom_store`'s
//! `LayoutCommand::begin` says why that is offered.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod defaults {
    //! Per-selector default group placement for newly created sessions.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Clear a placement default, so newly created sessions matching this
        /// selector fall through to a broader default (or the fallback origin `*`,
        /// which cannot itself be removed).
        #[operation(id = "session_layout.defaults.delete", actor = User, scope = Global,
                    risk = Write, default = custom, render = custom,
                    cli = "sessions layout default-delete")]
        pub struct Input {
            /// Which kind of selector the default to clear matches on: `origin`,
            /// `profile`, or `watch`.
            #[operand(string, positional)]
            pub selector_kind: SessionPlacementSelectorKind,
            #[operand(positional)]
            pub selector_value: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    selector_kind: SessionPlacementSelectorKind::Origin,
                    selector_value: String::new(),
                    expected_revision: None,
                }
            }
        }

        pub type Output = SessionLayoutView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set (or replace) the default group a newly created session lands in for
        /// one selector.
        #[operation(id = "session_layout.defaults.set", actor = User, scope = Global, risk = Write,
                    default = custom, render = custom, cli = "sessions layout default-set")]
        pub struct Input {
            /// Which kind of selector this default matches on: `origin`,
            /// `profile`, or `watch`.
            #[operand(string, positional)]
            pub selector_kind: SessionPlacementSelectorKind,
            #[operand(positional)]
            pub selector_value: String,
            /// The group matching sessions land in.
            #[operand(long = "to")]
            pub group_id: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    selector_kind: SessionPlacementSelectorKind::Origin,
                    selector_value: String::new(),
                    group_id: String::new(),
                    expected_revision: None,
                }
            }
        }

        pub type Output = SessionLayoutView;
    }
}

pub mod events {
    use super::prelude::*;

    /// Subscribe to layout changes as other dashboard tabs make them.
    #[operation(id = "session_layout.events", actor = User, scope = Global, risk = Read,
                io = Stream)]
    pub struct Input {}

    pub type Output = ();
}

pub mod get {
    use super::prelude::*;

    /// The signed-in operator's shared session-dashboard layout: spaces, groups,
    /// session placements, and per-selector placement defaults.
    #[operation(id = "session_layout.get", actor = User, scope = Global, risk = Read,
                render = custom, cli = "sessions layout show")]
    pub struct Input {}

    pub type Output = SessionLayoutView;
}

pub mod groups {
    //! Groups of session placements within one space.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Create a new group within a space.
        #[operation(id = "session_layout.groups.create", actor = User, scope = Global, risk = Write,
                    render = custom, cli = "sessions layout group-add")]
        pub struct Input {
            /// The space the group is created in.
            #[operand(positional)]
            pub space_id: String,
            #[operand(positional)]
            pub name: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }

    pub mod delete {
        use super::prelude::*;

        /// Delete a group. Deleting a group never deletes sessions:
        /// `destination_group_id` is required whenever the group owns placements or
        /// default-placement selectors, and its contents move there atomically.
        #[operation(id = "session_layout.groups.delete", actor = User, scope = Global,
                    risk = Destructive, render = custom, cli = "sessions layout group-delete")]
        pub struct Input {
            /// The group being deleted.
            #[operand(positional)]
            pub id: String,
            /// Where the group's sessions and placement defaults land. Required
            /// unless the group is empty.
            #[operand(long = "to")]
            pub destination_group_id: Option<String>,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }

    pub mod preference {
        //! Per-operator disclosure state (collapsed/expanded) for one group.
        pub(super) use super::prelude;
        pub mod set {
            use super::prelude::*;

            /// Set whether one group is collapsed in the caller's own dashboard.
            ///
            /// Unlike its bundle siblings this carries no `expected_revision`: it is a
            /// per-operator disclosure preference (`user_session_group_state`), not
            /// shared layout state another dashboard tab could race to change, so there
            /// is nothing to guard against.
            ///
            /// Also unlike its siblings it declares no `cli`: the command line spells
            /// it as two commands, `collapse` and `expand`, and one declaration is one
            /// invocation. Both stay hand-written in `bin/loom.rs`.
            #[operation(id = "session_layout.groups.preference.set", actor = User, scope = Global,
                        risk = Write, render = custom)]
            pub struct Input {
                /// The group whose disclosure state is being set.
                pub id: String,
                pub collapsed: bool,
            }

            pub type Output = SessionLayoutView;
        }
    }

    pub mod update {
        use super::prelude::*;

        /// Rename a group.
        #[operation(id = "session_layout.groups.update", actor = User, scope = Global, risk = Write,
                    render = custom, cli = "sessions layout group-rename")]
        pub struct Input {
            /// The group being renamed.
            #[operand(positional)]
            pub id: String,
            #[operand(positional)]
            pub name: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }
}

pub mod r#move {
    use super::prelude::*;

    /// Atomically move one or more sessions to an exact insertion point within a
    /// group.
    #[operation(id = "session_layout.move", actor = User, scope = Global, risk = Write,
                render = custom, cli = "sessions layout move")]
    pub struct Input {
        /// The sessions to move, in the order they should land.
        #[operand(positional)]
        pub session_ids: Vec<String>,
        /// The group they move into.
        #[operand(long = "to")]
        pub destination_group_id: String,
        /// Insert before this session in the destination group; omitted appends
        /// to the end.
        #[operand(long = "before")]
        pub before_session_id: Option<String>,
        /// Layout revision to guard against.
        #[operand(long = "revision")]
        pub expected_revision: Option<i64>,
    }

    pub type Output = SessionLayoutView;
}
pub mod reorder {
    use super::prelude::*;

    /// Reorder one space, or one group (optionally into another space).
    #[operation(id = "session_layout.reorder", actor = User, scope = Global, risk = Write,
                default = custom, render = custom, cli = "sessions layout reorder")]
    pub struct Input {
        /// Whether `id` names a `space` or a `group`.
        #[operand(string, positional)]
        pub kind: SessionLayoutItemKind,
        /// The space or group being repositioned.
        #[operand(positional)]
        pub id: String,
        /// Insert before this sibling; omitted moves to the end.
        #[operand(long = "before")]
        pub before_id: Option<String>,
        /// For a group, move it into this space; omitted keeps its current space.
        #[operand(long = "space")]
        pub destination_space_id: Option<String>,
        /// Layout revision to guard against.
        #[operand(long = "revision")]
        pub expected_revision: Option<i64>,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                kind: SessionLayoutItemKind::Space,
                id: String::new(),
                before_id: None,
                destination_space_id: None,
                expected_revision: None,
            }
        }
    }

    pub type Output = SessionLayoutView;
}

pub mod restore {
    use super::prelude::*;

    /// Atomically restore the complete membership and order of a set of groups.
    ///
    /// The supplied groups must cover exactly the sessions currently placed in
    /// those groups, so an undo fails as a stale whole instead of partially
    /// overwriting an intervening placement.
    #[operation(id = "session_layout.restore", actor = User, scope = Global, risk = Write,
                render = custom, cli = "sessions layout restore")]
    pub struct Input {
        /// A JSON array of `{"group_id":"…","session_ids":["…"]}` objects.
        #[operand(json)]
        pub groups: Vec<SessionGroupOrderReq>,
        /// Layout revision to guard against.
        #[operand(long = "revision")]
        pub expected_revision: Option<i64>,
    }

    pub type Output = SessionLayoutView;
}

pub mod spaces {
    //! Top-level containers in a session layout.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Create a new top-level space, seeded with an "Inbox" group.
        #[operation(id = "session_layout.spaces.create", actor = User, scope = Global, risk = Write,
                    render = custom, cli = "sessions layout space-add")]
        pub struct Input {
            #[operand(positional)]
            pub name: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }

    pub mod delete {
        use super::prelude::*;

        /// Delete a space. Deleting a non-empty space atomically moves its sessions
        /// and placement defaults to `destination_group_id`, which is required
        /// unless the space is empty. The last remaining space cannot be deleted.
        #[operation(id = "session_layout.spaces.delete", actor = User, scope = Global,
                    risk = Destructive, render = custom, cli = "sessions layout space-delete")]
        pub struct Input {
            /// The space being deleted.
            #[operand(positional)]
            pub id: String,
            /// Where the space's sessions and placement defaults land. Required
            /// unless the space is empty.
            #[operand(long = "to")]
            pub destination_group_id: Option<String>,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }

    pub mod update {
        use super::prelude::*;

        /// Rename a space.
        #[operation(id = "session_layout.spaces.update", actor = User, scope = Global, risk = Write,
                    render = custom, cli = "sessions layout space-rename")]
        pub struct Input {
            /// The space being renamed.
            #[operand(positional)]
            pub id: String,
            #[operand(positional)]
            pub name: String,
            /// Layout revision to guard against.
            #[operand(long = "revision")]
            pub expected_revision: Option<i64>,
        }

        pub type Output = SessionLayoutView;
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    get::SPEC,
    spaces::create::SPEC,
    spaces::update::SPEC,
    spaces::delete::SPEC,
    groups::create::SPEC,
    groups::update::SPEC,
    groups::delete::SPEC,
    groups::preference::set::SPEC,
    r#move::SPEC,
    reorder::SPEC,
    restore::SPEC,
    defaults::set::SPEC,
    defaults::delete::SPEC,
    events::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        // The `#[operation(...)]` macro derives an operation's `bundle` field
        // from its id's first dotted segment with `_` replaced by `-` (see
        // `loom-api-macros/src/operation.rs`), so this must match the derived
        // name, not the module's directory name.
        name: "session-layout",
        label: "Session layout",
        operations: OPERATIONS,
    }
}
