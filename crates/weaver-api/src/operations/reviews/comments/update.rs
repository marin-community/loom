use super::prelude::*;

/// Edit a draft review comment's text, or replace its anchor.
///
/// Replacing the anchor requires `subject_version`, `anchor_kind`, and
/// `anchor` together — a partial anchor replacement is rejected.
///
/// Operator-only. Rejected once the review has left `draft` status.
#[operation(
    id = "reviews.comments.update",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Update;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The review the comment belongs to.
    #[operand(positional)]
    pub id: i64,
    /// The comment to update.
    #[operand(positional)]
    pub comment_id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
    pub body: Option<String>,
    /// The subject version the replacement anchor was taken against.
    /// Required together with `anchor_kind` and `anchor`.
    pub subject_version: Option<String>,
    #[operand(json, default = None)]
    pub anchor_kind: Option<ReviewAnchorKindDto>,
    #[operand(json, default = None)]
    pub anchor: Option<ReviewAnchorDto>,
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
