use super::prelude::*;

/// Submit a review's draft, delivering its structured feedback into the
/// reviewed session's own conversation.
///
/// Operator-only, same reasoning as `reviews.comments.create` — only the
/// review's creator may submit it. See `creator_review` in
/// `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.submit",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Submit;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The review to submit.
    #[operand(positional)]
    pub id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
    /// Acknowledge that the review's subject moved since it was drafted, and
    /// submit against the newer version anyway.
    #[operand(default = false)]
    pub acknowledge_outdated: bool,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
