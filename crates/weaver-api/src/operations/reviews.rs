//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod comments {
    //! Anchored feedback comments on a review: added and edited while it is a
    //! private draft, resolved once it has been submitted.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Append an anchored feedback comment to a draft review.
        ///
        /// A review's comments stay private to the operator drafting them until
        /// `reviews.submit` delivers the whole thing.
        #[operation(id = "reviews.comments.create", actor = User, scope = Global, risk = Write,
                    default = custom)]
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
    }

    pub mod delete {
        use super::prelude::*;

        /// Remove a draft review comment.
        ///
        /// Limited to the review's own creator, and rejected once the review
        /// has left `draft` status.
        #[operation(id = "reviews.comments.delete", actor = User, scope = Global, risk = Write)]
        pub struct Input {
            /// The review the comment belongs to.
            #[operand(positional)]
            pub id: i64,
            /// The comment to delete.
            #[operand(positional)]
            pub comment_id: i64,
            /// Optimistic-concurrency guard on the review's draft revision.
            pub expected_revision: i64,
        }

        pub type Output = ReviewDto;
    }

    pub mod resolve {
        use super::prelude::*;

        /// Mark a comment on a submitted review resolved or unresolved.
        ///
        /// Any operator may resolve a comment on any submitted review — unlike the
        /// other `reviews.comments.*` operations, this one is not limited to the
        /// creator.
        #[operation(id = "reviews.comments.resolve", actor = User, scope = Global, risk = Write)]
        pub struct Input {
            /// The submitted review the comment belongs to.
            #[operand(positional)]
            pub id: i64,
            /// The comment to resolve or unresolve.
            #[operand(positional)]
            pub comment_id: i64,
            pub resolved: bool,
        }

        pub type Output = ReviewCommentDto;
    }

    pub mod update {
        use super::prelude::*;

        /// Edit a draft review comment's text, or replace its anchor.
        ///
        /// Replacing the anchor requires `subject_version`, `anchor_kind`, and
        /// `anchor` together — a partial anchor replacement is rejected.
        ///
        /// Operator-only. Rejected once the review has left `draft` status.
        #[operation(id = "reviews.comments.update", actor = User, scope = Global, risk = Write)]
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
    }
}

pub mod create {
    use super::prelude::*;

    /// Create or reuse a draft review over a session's artifact or its
    /// change-set, seeding it against the currently-visible subject version.
    #[operation(id = "reviews.create", actor = User, scope = Session, risk = Write,
                default = custom)]
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
}

pub mod discard {
    use super::prelude::*;

    /// Permanently discard a draft review.
    ///
    /// Limited to the review's own creator, and rejected once the review has left
    /// `draft` status.
    #[operation(id = "reviews.discard", actor = User, scope = Global, risk = Destructive)]
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
}

pub mod get {
    use super::prelude::*;

    /// Fetch a durable review by id, refreshed against its subject's current
    /// version.
    ///
    /// A submitted review is visible to any operator; a draft only to the
    /// operator who created it.
    #[operation(id = "reviews.get", actor = User, scope = Global, risk = Read)]
    pub struct Input {
        /// The review to fetch.
        #[operand(positional)]
        pub id: i64,
    }

    pub type Output = ReviewDto;
}

pub mod list {
    use super::prelude::*;

    /// List a session's reviews for one subject — an artifact or its change-set.
    ///
    /// Reachable by both the reviewed session's own credential and a human
    /// operator: sessions may see submitted feedback on their own work, but not
    /// draft reviews from other operators.
    #[operation(id = "reviews.list", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/artifacts/read@v1"], default = custom)]
    pub struct Input {
        pub subject_kind: ReviewSubjectKindDto,
        /// The artifact name for `subject_kind = "artifact"`, or `"changes"` for
        /// `subject_kind = "changes"`.
        pub subject_key: String,
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
}

pub mod retarget {
    use super::prelude::*;

    /// Retarget a draft review's subject onto its current version — an
    /// artifact's latest revision, or the branch's current change-set — in one
    /// step, without touching anything else.
    ///
    /// Limited to the review's own creator, and rejected once the review has left
    /// `draft` status.
    #[operation(id = "reviews.retarget", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        /// The draft review to retarget.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision.
        pub expected_revision: i64,
    }

    pub type Output = ReviewDto;
}

pub mod retry_delivery {
    use super::prelude::*;

    /// Retry a submitted review's delivery after it failed.
    ///
    /// Any operator may retry delivery of any submitted review — unlike
    /// `reviews.submit`, this one is not limited to the creator.
    #[operation(id = "reviews.retry_delivery", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        /// The submitted review whose delivery failed.
        #[operand(positional)]
        pub id: i64,
    }

    pub type Output = ReviewDto;
}

pub mod submit {
    use super::prelude::*;

    /// Submit a review's draft, delivering its structured feedback into the
    /// reviewed session's own conversation.
    ///
    /// Limited to the review's own creator.
    #[operation(id = "reviews.submit", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        /// The review to submit.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision.
        pub expected_revision: i64,
        /// Acknowledge that the review's subject moved since it was drafted, and
        /// submit against the newer version anyway.
        #[operand(default = false)]
        pub acknowledge_outdated: bool,
    }

    pub type Output = ReviewDto;
}

pub mod update {
    use super::prelude::*;

    /// Edit a draft review's summary, or retarget it onto a caller-supplied
    /// subject version.
    ///
    /// Limited to the review's own creator, and rejected once the review has left
    /// `draft` status.
    #[operation(id = "reviews.update", actor = User, scope = Global, risk = Write)]
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
}

static OPERATIONS: &[&OperationSpec] = &[
    get::SPEC,
    update::SPEC,
    discard::SPEC,
    retarget::SPEC,
    list::SPEC,
    create::SPEC,
    comments::create::SPEC,
    comments::update::SPEC,
    comments::delete::SPEC,
    comments::resolve::SPEC,
    submit::SPEC,
    retry_delivery::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "reviews",
        label: "Reviews",
        operations: OPERATIONS,
    }
}
