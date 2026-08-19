use super::prelude::*;

/// Mark a comment on a submitted review resolved or unresolved.
///
/// Operator-only, but — unlike the other `reviews.comments.*` operations —
/// not limited to the review's own creator: any human operator may resolve a
/// comment on any submitted review. See `submitted_operator_review` in
/// `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.comments.resolve",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Resolve;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The submitted review the comment belongs to.
    #[operand(positional)]
    pub id: i64,
    /// The comment to resolve or unresolve.
    #[operand(positional)]
    pub comment_id: i64,
    pub resolved: bool,
}

pub type Output = ReviewCommentDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
