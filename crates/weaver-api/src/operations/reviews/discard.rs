use super::prelude::*;

/// Permanently discard a draft review.
///
/// Operator-only, limited to the review's own creator. Rejected once the
/// review has left `draft` status.
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    pub discarded: bool,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
