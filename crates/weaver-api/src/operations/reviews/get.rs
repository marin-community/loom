use super::prelude::*;

/// Fetch a durable review by id, refreshed against its subject's current
/// version.
///
/// Operator-only: a submitted review is visible to any human operator, and a
/// draft only to the operator who created it. See `require_operator` and
/// `review::get_visible` in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The review to fetch.
    #[operand(positional)]
    pub id: i64,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
