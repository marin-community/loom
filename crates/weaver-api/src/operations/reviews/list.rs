use super::prelude::*;

/// List a session's reviews for one subject — an artifact or its change-set.
///
/// Reachable by both the reviewed session's own credential and a human
/// operator: sessions may see submitted feedback on their own work, but not
/// draft reviews from other operators.
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
