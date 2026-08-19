//! `artifacts.threads.comment` — start a thread or reply to one.
//!
//! The old registry could only express this as one endpoint whose fields were
//! all individually optional (`thread_id`? `base_rev`? `quote`?) because its
//! `ArgumentSpec` had five scalar kinds and no way to say "one of these two
//! shapes." That let a caller send neither a `thread_id` nor a `base_rev` and
//! find out only at request time. `CommentTarget` is a real tagged union —
//! the same fix `issues.actions` applies to its own `action` field.

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
