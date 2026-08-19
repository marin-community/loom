use super::prelude::*;

/// List a session's reviews for one subject — an artifact, or its
/// change-set — merging live durable reviews with legacy artifact-thread
/// reviews for backward compatibility.
///
/// The frontend's `listArtifactReviews` and `listChangesReviews` are both
/// this one legacy route (`GET /sessions/{id}/reviews`), discriminated by
/// `subject_kind`/`subject_key`, not two separate routes — so one operation
/// with a discriminating operand pair models it honestly instead of
/// splitting it in two.
///
/// Reachable by the reviewed session's own credential as well as a human
/// operator: `list_for` in `crates/loom/src/web/reviews.rs` narrows draft
/// visibility by `principal.is_human()` rather than rejecting a session
/// credential outright (a session may see submitted feedback on its own
/// work, never another operator's private draft), and the legacy path
/// allowlist in `web/auth.rs` (`Grant::Session` arm, the
/// `segments.first() == Some(&"sessions")` rule) already lets a session
/// reach its own `/sessions/{id}/reviews`.
#[operation(
    id = "reviews.list",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
)]
pub struct List;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    pub subject_kind: ReviewSubjectKindDto,
    /// The artifact name for `subject_kind = "artifact"`, or `"changes"` for
    /// `subject_kind = "changes"`.
    pub subject_key: String,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            subject_kind: ReviewSubjectKindDto::Artifact,
            subject_key: String::new(),
            session: String::new(),
        }
    }
}

pub type Output = Vec<ReviewDto>;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
