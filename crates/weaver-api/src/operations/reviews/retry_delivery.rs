use super::prelude::*;

/// Retry a submitted review's delivery after it failed.
///
/// Operator-only, and — unlike `reviews.comments.create` and
/// `reviews.submit` — not limited to the review's own creator: any human
/// operator may retry delivery of any submitted review. See
/// `submitted_operator_review` in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.retry_delivery",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct RetryDelivery;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The submitted review whose delivery failed.
    #[operand(positional)]
    pub id: i64,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
