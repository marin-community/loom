use super::prelude::*;

/// Retarget a draft review's subject onto its current version — an
/// artifact's latest revision, or the branch's current change-set — in one
/// step, without touching anything else.
///
/// Operator-only, and limited to the review's own creator — same reasoning
/// as `reviews.comments.create`. Rejected once the review has left `draft`
/// status. See `creator_review` in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.retarget",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Retarget;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The draft review to retarget.
    #[operand(positional)]
    pub id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
