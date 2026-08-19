use super::prelude::*;

/// Permanently discard a draft review.
///
/// Operator-only, and limited to the review's own creator — same reasoning
/// as `reviews.comments.create`. Rejected once the review has left `draft`
/// status. See `creator_review` in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.discard",
    actor = User,
    scope = Global,
    risk = Destructive,
    grants = [],
)]
pub struct Discard;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The draft review to discard.
    #[operand(positional)]
    pub id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
}

/// The legacy route's exact response shape — a discard confirmation and
/// nothing else, per the porting rule against widening a response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    pub discarded: bool,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
