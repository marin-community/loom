use super::prelude::*;

/// Create or reuse a draft review over a session's artifact or its
/// change-set, seeding it against the currently-visible subject version.
///
/// Operator-only: a review's draft belongs to the human operator who starts it,
/// so a session credential may not start one.
///
/// `session` names the session whose artifact or change-set is under review,
/// not the caller's own.
#[operation(
    id = "reviews.create",
    actor = User,
    scope = Session,
    risk = Write,
    grants = [],
)]
pub struct Create;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The session whose artifact or change-set is under review.
    #[operand(positional)]
    pub session: String,
    pub subject_kind: ReviewSubjectKindDto,
    /// Artifact name for `subject_kind = "artifact"`, or `"changes"` for
    /// `subject_kind = "changes"`.
    pub subject_key: String,
    /// The subject version this draft starts from: an artifact revision
    /// number, or the current change-set version (which must match exactly
    /// for a changes review).
    pub subject_version: String,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            session: String::new(),
            subject_kind: ReviewSubjectKindDto::Artifact,
            subject_key: String::new(),
            subject_version: String::new(),
        }
    }
}

pub type Output = ReviewDto;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
