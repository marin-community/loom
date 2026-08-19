//! Versioned deliverables.
//!
//! Named documents an agent (or the user) writes to weaver — plus the
//! anchored review threads discussed against them.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod delete {
    use super::prelude::*;

    /// Delete an artifact and its complete revision history.
    #[operation(
    id = "artifacts.delete",
    actor = SessionSelf,
    scope = Branch,
    risk = Destructive,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts delete",
    mcp = "loom_artifact::delete",
)]
    pub struct Delete;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        /// When true, delete the repository-shared artifact. By default, delete
        /// this branch's own copy.
        #[operand(default = false)]
        pub repo: bool,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactDeleteResult;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod get {
    use super::prelude::*;

    /// Read one artifact or immutable revision.
    #[operation(
    id = "artifacts.get",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts get",
    mcp = "loom_artifact::get",
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        /// Select an immutable past revision instead of the latest.
        pub rev: Option<i64>,
        /// When true, read the repository-shared artifact. By default, resolve
        /// this branch's own copy first.
        #[operand(default = false)]
        pub repo: bool,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod history {
    use super::prelude::*;

    /// List immutable artifact revisions.
    #[operation(
    id = "artifacts.history",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts history",
    mcp = "loom_artifact::history",
)]
    pub struct History;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        /// When true, list the repository-shared artifact's history. By default,
        /// list this branch's own copy.
        #[operand(default = false)]
        pub repo: bool,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = Vec<ArtifactVersion>;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod list {
    use super::prelude::*;

    /// List branch and repository-scoped artifacts.
    #[operation(
    id = "artifacts.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts list",
    mcp = "loom_artifact::list",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// When true, list every artifact in the repository. By default, list
        /// only this branch's own artifacts and the repository-shared ones.
        #[operand(default = false)]
        pub repo: bool,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = Vec<ArtifactMeta>;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod raw {
    use super::prelude::*;

    /// An image artifact's decoded bytes, for an `<img src>`.
    ///
    /// `io = Download` because the browser's image loader issues a `GET` and
    /// expects `image/png` — a bare binary payload, not a JSON envelope.
    /// [`super::get`] provides the same artifact as JSON; the two operations offer
    /// different encodings of the same data.
    #[operation(
    id = "artifacts.raw",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    io = Download,
)]
    pub struct Raw;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        //
        // `serde(default)` because a download's operands arrive in the query string,
        // which axum extracts before any default-filling could run.
        #[serde(default)]
        #[operand(default = String::new())]
        pub name: String,
        /// Pin an immutable past revision instead of the latest.
        #[serde(default)]
        pub rev: Option<i64>,
        /// Resolved from the calling session; not something a caller supplies.
        #[serde(default)]
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ();

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod threads {
    //! Anchored review threads discussed against an artifact's content.
    pub(super) use super::prelude;
    pub mod comment {
        //! `artifacts.threads.comment` — start a thread or reply to one.
        //!
        //! `CommentTarget` is a real tagged union, enforcing at compile time that
        //! a caller provides either a base revision and anchor (to start a new thread)
        //! or a thread id (to reply to an existing one). This matches the pattern
        //! `issues.actions` applies to its own `action` field.

        use super::prelude::*;

        /// Start or reply to an artifact review thread.
        #[operation(
    id = "artifacts.threads.comment",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts comment",
    mcp = "loom_artifact::comment",
)]
        pub struct Comment;

        /// Where a comment attaches: a fresh anchored thread, or a reply to one
        /// already open.
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case", tag = "kind")]
        pub enum CommentTarget {
            /// Open a new thread anchored to a quoted span of the artifact.
            New {
                /// The artifact revision the anchor was taken from.
                base_rev: i64,
                anchor: AnchorDto,
            },
            /// Reply to an already-open thread.
            Reply { thread_id: i64 },
        }

        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            pub name: String,
            /// The comment text.
            #[operand(positional)]
            pub body: String,
            /// Start a new thread or reply to one. On the command line this takes a
            /// JSON object, because a tagged union is not a flag.
            #[operand(json)]
            pub target: CommentTarget,
            /// Resolved from the calling session; not something a caller supplies.
            #[operand(context)]
            pub branch: String,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    name: String::new(),
                    body: String::new(),
                    target: CommentTarget::Reply { thread_id: 0 },
                    branch: String::new(),
                }
            }
        }

        pub type Output = ThreadDto;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Branch(&self.branch)
            }
        }
    }

    pub mod list {
        use super::prelude::*;

        /// List anchored artifact review threads.
        #[operation(
    id = "artifacts.threads.list",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    cli = "artifacts threads",
    mcp = "loom_artifact::threads",
)]
        pub struct List;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            pub name: String,
            /// When true, list only unresolved threads. By default, include all threads.
            /// Resolved threads appear collapsed in the dashboard, not hidden.
            #[operand(default = false)]
            pub open_only: bool,
            /// Resolved from the calling session; not something a caller supplies.
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<ThreadDto>;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Branch(&self.branch)
            }
        }
    }

    pub mod resolve {
        use super::prelude::*;

        /// Resolve an artifact review thread.
        #[operation(
    id = "artifacts.threads.resolve",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts resolve",
    mcp = "loom_artifact::resolve",
)]
        pub struct Resolve;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            pub name: String,
            /// The thread to resolve.
            #[operand(positional)]
            pub thread_id: i64,
            /// Resolved from the calling session; not something a caller supplies.
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = ThreadDto;

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Branch(&self.branch)
            }
        }
    }
}

pub mod url {
    use super::prelude::*;

    /// The externally-visible dashboard deep-link for an artifact.
    ///
    /// The agent that just wrote the artifact holds only the loopback (or
    /// wildcard) `$WEAVER_API` it was handed, and a `http://0.0.0.0:7878/…` link
    /// printed after a write is useless to whoever reads it. Only the server
    /// knows the externally-visible origin (the operator's `auth.base_url`, else
    /// the request's own Host), so resolving it is the server's job — the twin of
    /// `sessions.url`, whose `SessionUrlView` (`{url}`) this reuses unchanged.
    #[operation(
    id = "artifacts.url",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
)]
    pub struct Url;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = SessionUrlView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

pub mod write {
    use super::prelude::*;

    /// Create an artifact or append a guarded revision.
    ///
    /// The API accepts `content` as a JSON string. The CLI tool supports reading
    /// from a file or stdin via `#[operand(from_file)]` for convenience, but this
    /// is a client-side transformation — the wire format remains a JSON string.
    #[operation(
    id = "artifacts.write",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts write",
    mcp = "loom_artifact::write",
)]
    pub struct Write;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        /// The artifact body. On the command line this names a file, or `-`/omitted
        /// to read stdin.
        #[operand(positional, from_file)]
        pub content: String,
        /// Display title. Defaults to the existing title, or the name for a new
        /// artifact.
        pub title: Option<String>,
        /// Content kind, e.g. `markdown` or `image`.
        ///
        /// When omitted, the artifact keeps its current kind. This must be optional
        /// because a default value would silently change existing `plan` or `image`
        /// artifacts to markdown on every update that omits this field.
        pub kind: Option<String>,
        /// Optimistic-concurrency guard: `0` guards creation; a later revision
        /// number rejects a stale edit instead of silently overwriting it.
        pub base_rev: Option<i64>,
        /// Write the repository-shared artifact instead of this branch's own
        /// copy.
        #[operand(default = false)]
        pub repo: bool,
        /// Resolved from the calling session; not something a caller supplies.
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactView;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Branch(&self.branch)
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <raw::Raw as Operation>::SPEC,
    <write::Write as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <history::History as Operation>::SPEC,
    <url::Url as Operation>::SPEC,
    <threads::list::List as Operation>::SPEC,
    <threads::comment::Comment as Operation>::SPEC,
    <threads::resolve::Resolve as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "artifacts",
        label: "Versioned deliverables",
        operations: OPERATIONS,
    }
}
