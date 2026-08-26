//! Versioned deliverables.
//!
//! Named documents an agent (or the user) writes to weaver — plus the
//! anchored review threads discussed against them.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod delete {
    use super::prelude::*;

    /// Delete an artifact and its complete revision history.
    #[operation(id = "artifacts.delete", actor = SessionSelf, scope = Branch, risk = Destructive,
                grants = ["loom/artifacts/write@v1"], cli = "artifacts delete")]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        #[schemars(length(min = 1, max = 255))]
        pub name: String,
        /// When true, delete the repository-shared artifact. By default, delete
        /// this branch's own copy.
        #[operand(default = false)]
        pub repo: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactDeleteResult;
}

pub mod get {
    use super::prelude::*;

    /// Read one artifact or immutable revision.
    #[operation(id = "artifacts.get", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/artifacts/read@v1"], cli = "artifacts get", cli_alias = "show",
                view = View, render = custom)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        #[schemars(length(min = 1, max = 255))]
        pub name: String,
        /// Select an immutable past revision instead of the latest.
        #[schemars(range(min = 1))]
        pub rev: Option<i64>,
        /// When true, read the repository-shared artifact. By default, resolve
        /// this branch's own copy first.
        #[operand(default = false)]
        pub repo: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactView;

    /// Presentation flags. The response always carries both the envelope and
    /// the content; this only chooses which of them gets printed.
    #[derive(Debug, Clone, Default, Deserialize, View)]
    pub struct View {
        /// Print the envelope metadata instead of the content.
        pub meta: bool,
    }
}

pub mod history {
    use super::prelude::*;

    /// List immutable artifact revisions.
    #[operation(id = "artifacts.history", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/artifacts/read@v1"], cli = "artifacts history", render = custom)]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        #[schemars(length(min = 1, max = 255))]
        pub name: String,
        /// When true, list the repository-shared artifact's history. By default,
        /// list this branch's own copy.
        #[operand(default = false)]
        pub repo: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = Vec<ArtifactVersion>;
}

pub mod list {
    use super::prelude::*;

    /// List branch and repository-scoped artifacts.
    #[operation(id = "artifacts.list", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/artifacts/read@v1"], cli = "artifacts list", cli_alias = "ls",
                render = custom)]
    pub struct Input {
        /// When true, list every artifact in the repository. By default, list
        /// only this branch's own artifacts and the repository-shared ones.
        #[operand(default = false)]
        pub repo: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = Vec<ArtifactMeta>;
}

pub mod raw {
    use super::prelude::*;

    /// An image artifact's decoded bytes, for an `<img src>`.
    ///
    /// `io = Download`: the browser's image loader issues a `GET` and expects
    /// `image/png`. `artifacts.get` returns the same artifact as JSON.
    #[operation(id = "artifacts.raw", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/artifacts/read@v1"], io = Download)]
    pub struct Input {
        /// The artifact's name.
        #[operand(default = String::new())]
        #[schemars(length(min = 1, max = 255))]
        pub name: String,
        /// Pin an immutable past revision instead of the latest.
        #[schemars(range(min = 1))]
        pub rev: Option<i64>,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ();
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

        /// Where a comment attaches: a fresh anchored thread, or a reply to one
        /// already open.
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case", tag = "kind")]
        pub enum CommentTarget {
            /// Open a new thread anchored to a quoted span of the artifact.
            New {
                /// The artifact revision the anchor was taken from.
                #[schemars(range(min = 1))]
                base_rev: i64,
                anchor: AnchorDto,
            },
            /// Reply to an already-open thread.
            Reply {
                #[schemars(range(min = 1))]
                thread_id: i64,
            },
        }

        /// Start or reply to an artifact review thread.
        #[operation(id = "artifacts.threads.comment", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/artifacts/write@v1"], cli = "artifacts comment", default = custom)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            #[schemars(length(min = 1, max = 255))]
            pub name: String,
            /// The comment text.
            #[operand(positional)]
            #[schemars(length(min = 1))]
            pub body: String,
            /// Start a new thread or reply to one. On the command line this takes a
            /// JSON object, because a tagged union is not a flag.
            #[operand(json)]
            pub target: CommentTarget,
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
    }

    pub mod list {
        use super::prelude::*;

        /// List anchored artifact review threads.
        #[operation(id = "artifacts.threads.list", actor = SessionSelf, scope = Branch, risk = Read,
                    grants = ["loom/artifacts/read@v1"], cli = "artifacts threads", view = View,
                    render = custom)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            #[schemars(length(min = 1, max = 255))]
            pub name: String,
            /// When true, list only unresolved threads. By default, include all threads.
            /// Resolved threads appear collapsed in the dashboard, not hidden.
            // `skip_cli`: the command line spells this choice the other way
            // round — open threads by default, `--all` to widen — so offering
            // this one too would be two flags for one question. A `//` comment,
            // because a doc comment would carry CLI trivia into the MCP
            // argument schema and the generated frontend types.
            #[operand(default = false, skip_cli)]
            pub open_only: bool,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<ThreadDto>;

        /// Presentation flags. Every thread is fetched; this chooses which of
        /// them gets printed.
        #[derive(Debug, Clone, Default, Deserialize, View)]
        pub struct View {
            /// Show resolved and orphaned threads too, not just the open ones.
            pub all: bool,
        }
    }

    pub mod resolve {
        use super::prelude::*;

        /// Resolve an artifact review thread.
        #[operation(id = "artifacts.threads.resolve", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/artifacts/write@v1"], cli = "artifacts resolve",
                    render = custom)]
        pub struct Input {
            /// The artifact's name.
            #[operand(positional)]
            #[schemars(length(min = 1, max = 255))]
            pub name: String,
            /// The thread to resolve.
            #[operand(positional)]
            #[schemars(range(min = 1))]
            pub thread_id: i64,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = ThreadDto;
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
    #[operation(id = "artifacts.url", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/artifacts/read@v1"])]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        pub name: String,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = SessionUrlView;
}

pub mod write {
    use super::prelude::*;

    /// Create an artifact or append a guarded revision.
    ///
    /// The wire format is always a JSON string. Reading `content` from a file
    /// or stdin is a convenience the command line applies before sending.
    #[operation(id = "artifacts.write", actor = SessionSelf, scope = Branch, risk = Write,
                grants = ["loom/artifacts/write@v1"], cli = "artifacts write")]
    pub struct Input {
        /// The artifact's name.
        #[operand(positional)]
        #[schemars(length(min = 1, max = 255))]
        pub name: String,
        /// The artifact body. On the command line this names a file, or `-`/omitted
        /// to read stdin.
        #[operand(positional, from_file)]
        pub content: String,
        /// Display title. Defaults to the existing title, or the name for a new
        /// artifact.
        #[schemars(length(max = 4096))]
        pub title: Option<String>,
        /// Content kind, e.g. `markdown` or `image`.
        ///
        /// When omitted, the artifact keeps its current kind. This must be optional
        /// because a default value would silently change existing `plan` or `image`
        /// artifacts to markdown on every update that omits this field.
        #[schemars(length(min = 1))]
        pub kind: Option<String>,
        /// Optimistic-concurrency guard: `0` guards creation; a later revision
        /// number rejects a stale edit instead of silently overwriting it.
        #[schemars(range(min = 0))]
        pub base_rev: Option<i64>,
        /// Write the repository-shared artifact instead of this branch's own
        /// copy.
        #[operand(default = false)]
        pub repo: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ArtifactView;
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    get::SPEC,
    raw::SPEC,
    write::SPEC,
    delete::SPEC,
    history::SPEC,
    url::SPEC,
    threads::list::SPEC,
    threads::comment::SPEC,
    threads::resolve::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "artifacts",
        label: "Versioned deliverables",
        operations: OPERATIONS,
    }
}
