use super::prelude::*;

/// Append an anchored feedback comment to a draft review.
///
/// Operator-only: a review's comments are private to the human drafting them
/// until `reviews.submit` delivers the whole thing, so a session credential —
/// even the reviewed session's own — may not add one. See `require_operator`
/// in `crates/loom/src/web/reviews.rs`.
#[operation(
    id = "reviews.comments.create",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
pub struct Create;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The review to comment on.
    #[operand(positional)]
    pub id: i64,
    /// Optimistic-concurrency guard on the review's draft revision.
    pub expected_revision: i64,
    /// The subject version (artifact revision, or change-set version) the
    /// anchor was taken against.
    pub subject_version: String,
    #[operand(json)]
    pub anchor_kind: ReviewAnchorKindDto,
    #[operand(json)]
    pub anchor: ReviewAnchorDto,
    pub body: String,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            id: 0,
            expected_revision: 0,
            subject_version: String::new(),
            anchor_kind: ReviewAnchorKindDto::Text,
            anchor: ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                quote: String::new(),
                prefix: String::new(),
                suffix: String::new(),
                block_index: None,
            }),
            body: String::new(),
        }
    }
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
