//! The signed-in operator's shared session-dashboard layout: spaces, groups,
//! and the placement of sessions within them.
//!
//! This is dashboard state, not a session credential's surface — every
//! mutation is keyed off the calling human's own username and every write
//! carries an `expected_revision` optimistic-concurrency guard, since more
//! than one open dashboard tab can race to reorganize the same layout.

use super::registry::{Operation, OperationSpec};
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
        #[operation(
    id = "session_layout.defaults.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Delete;

        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// Which kind of selector the default to clear matches on.
            #[operand(json)]
            pub selector_kind: SessionPlacementSelectorKind,
            pub selector_value: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    selector_kind: SessionPlacementSelectorKind::Origin,
                    selector_value: String::new(),
                    expected_revision: 0,
                }
            }
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod set {
        use super::prelude::*;

        /// Set (or replace) the default group a newly created session lands in for
        /// one selector.
        #[operation(
    id = "session_layout.defaults.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Set;

        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// Which kind of selector this default matches on.
            #[operand(json)]
            pub selector_kind: SessionPlacementSelectorKind,
            pub selector_value: String,
            pub group_id: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    selector_kind: SessionPlacementSelectorKind::Origin,
                    selector_value: String::new(),
                    group_id: String::new(),
                    expected_revision: 0,
                }
            }
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod events {
    use super::prelude::*;

    /// Subscribe to layout changes as other dashboard tabs make them.
    ///
    /// `actor = User`: the layout is the signed-in operator's own dashboard state,
    /// and a session credential has never been able to read it.
    #[operation(
    id = "session_layout.events",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    io = Stream,
)]
    pub struct Events;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = ();

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod get {
    use super::prelude::*;

    /// The signed-in operator's shared session-dashboard layout: spaces, groups,
    /// session placements, and per-selector placement defaults.
    #[operation(
    id = "session_layout.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = SessionLayoutView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod groups {
    //! Groups of session placements within one space.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Create a new group within a space.
        #[operation(
    id = "session_layout.groups.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Create;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            pub space_id: String,
            pub name: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod delete {
        use super::prelude::*;

        /// Delete a group. Deleting a group never deletes sessions:
        /// `destination_group_id` is required whenever the group owns placements or
        /// default-placement selectors, and its contents move there atomically.
        #[operation(
    id = "session_layout.groups.delete",
    actor = User,
    scope = Global,
    risk = Destructive,
    grants = [],
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The group being deleted.
            pub id: String,
            /// Where the group's sessions and placement defaults land. Required
            /// unless the group is empty.
            pub destination_group_id: Option<String>,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
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
            #[operation(
    id = "session_layout.groups.preference.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
            pub struct Set;

            #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
            pub struct Input {
                /// The group whose disclosure state is being set.
                pub id: String,
                pub collapsed: bool,
            }

            pub type Output = SessionLayoutView;

            impl Scoped for Input {
                fn scope_ref(&self) -> ScopeRef<'_> {
                    ScopeRef::Global
                }
            }
        }
    }

    pub mod update {
        use super::prelude::*;

        /// Rename a group.
        #[operation(
    id = "session_layout.groups.update",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Update;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The group being renamed.
            pub id: String,
            pub name: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

pub mod r#move {
    use super::prelude::*;

    /// Atomically move one or more sessions to an exact insertion point within a
    /// group.
    #[operation(
    id = "session_layout.move",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Move;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        pub session_ids: Vec<String>,
        pub destination_group_id: String,
        /// Insert before this session in the destination group; omitted appends
        /// to the end.
        pub before_session_id: Option<String>,
        /// Optimistic-concurrency guard: the layout revision this call was
        /// composed against. Stale calls are rejected to prevent concurrent
        /// edit conflicts.
        pub expected_revision: i64,
    }

    pub type Output = SessionLayoutView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}
pub mod reorder {
    use super::prelude::*;

    /// Reorder one space, or one group (optionally into another space).
    #[operation(
    id = "session_layout.reorder",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Reorder;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Whether `id` names a space or a group.
        #[operand(json)]
        pub kind: SessionLayoutItemKind,
        /// The space or group being repositioned.
        pub id: String,
        /// Insert before this sibling; omitted moves to the end.
        pub before_id: Option<String>,
        /// For a group, move it into this space; omitted keeps its current space.
        pub destination_space_id: Option<String>,
        /// Optimistic-concurrency guard: the layout revision this call was
        /// composed against. Stale calls are rejected to prevent concurrent
        /// edit conflicts.
        pub expected_revision: i64,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                kind: SessionLayoutItemKind::Space,
                id: String::new(),
                before_id: None,
                destination_space_id: None,
                expected_revision: 0,
            }
        }
    }

    pub type Output = SessionLayoutView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod restore {
    use super::prelude::*;

    /// Atomically restore the complete membership and order of a set of groups.
    ///
    /// The supplied groups must cover exactly the sessions currently placed in
    /// those groups, so an undo fails as a stale whole instead of partially
    /// overwriting an intervening placement.
    #[operation(
    id = "session_layout.restore",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Restore;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        #[operand(json)]
        pub groups: Vec<SessionGroupOrderReq>,
        /// Optimistic-concurrency guard: the layout revision this call was
        /// composed against. Stale calls are rejected to prevent concurrent
        /// edit conflicts.
        pub expected_revision: i64,
    }

    pub type Output = SessionLayoutView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod spaces {
    //! Top-level containers in a session layout.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Create a new top-level space, seeded with an "Inbox" group.
        #[operation(
    id = "session_layout.spaces.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Create;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            pub name: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod delete {
        use super::prelude::*;

        /// Delete a space. Deleting a non-empty space atomically moves its sessions
        /// and placement defaults to `destination_group_id`, which is required
        /// unless the space is empty. The last remaining space cannot be deleted.
        #[operation(
    id = "session_layout.spaces.delete",
    actor = User,
    scope = Global,
    risk = Destructive,
    grants = [],
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The space being deleted.
            pub id: String,
            /// Where the space's sessions and placement defaults land. Required
            /// unless the space is empty.
            pub destination_group_id: Option<String>,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Stale calls are rejected to prevent concurrent
            /// edit conflicts.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod update {
        use super::prelude::*;

        /// Rename a space.
        #[operation(
    id = "session_layout.spaces.update",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Update;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The space being renamed.
            pub id: String,
            pub name: String,
            /// Optimistic-concurrency guard: the layout revision this call was
            /// composed against. Rejects stale callers to prevent clobbering concurrent
            /// edits from other dashboard tabs.
            pub expected_revision: i64,
        }

        pub type Output = SessionLayoutView;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <spaces::create::Create as Operation>::SPEC,
    <spaces::update::Update as Operation>::SPEC,
    <spaces::delete::Delete as Operation>::SPEC,
    <groups::create::Create as Operation>::SPEC,
    <groups::update::Update as Operation>::SPEC,
    <groups::delete::Delete as Operation>::SPEC,
    <groups::preference::set::Set as Operation>::SPEC,
    <r#move::Move as Operation>::SPEC,
    <reorder::Reorder as Operation>::SPEC,
    <restore::Restore as Operation>::SPEC,
    <defaults::set::Set as Operation>::SPEC,
    <defaults::delete::Delete as Operation>::SPEC,
    <events::Events as Operation>::SPEC,
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
