use super::prelude::*;

/// Edit a draft review's summary, or retarget it onto a caller-supplied
/// subject version.
///
/// Operator-only, and limited to the review's own creator — same reasoning
/// as `reviews.comments.create`. Rejected once the review has left `draft`
/// status. See `creator_review` in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.update",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The draft review to update.
    #[operand(positional)]
    pub id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
    pub summary: Option<String>,
    /// A newer subject version to retarget onto: an artifact revision number
    /// for an artifact review, or the current change-set version for a
    /// changes review (which must match the current version exactly).
    pub subject_version: Option<String>,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
