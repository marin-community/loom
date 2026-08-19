use super::prelude::*;

/// Remove a draft review comment.
///
/// Operator-only, same reasoning as `reviews.comments.create`. Rejected once
/// the review has left `draft` status. See `creator_review` in
/// `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.comments.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The review the comment belongs to.
    #[operand(positional)]
    pub id: i64,
    /// The comment to delete.
    #[operand(positional)]
    pub comment_id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
