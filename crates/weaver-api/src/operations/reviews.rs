//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation.

use super::registry::{Operation, OperationSpec};
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
    }

    pub mod delete {
        use super::prelude::*;

        /// Remove a draft review comment.
        ///
        /// Operator-only, same reasoning as `reviews.comments.create`. Rejected once
        /// the review has left `draft` status. See `creator_review` in
        /// `crates/loom/src/web/reviews.rs`.
        #[operation(
    id = "reviews.comments.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
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

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod resolve {
        use super::prelude::*;

        /// Mark a comment on a submitted review resolved or unresolved.
        ///
        /// Operator-only, but — unlike the other `reviews.comments.*` operations —
        /// not limited to the review's own creator: any human operator may resolve a
        /// comment on any submitted review. See `submitted_operator_review` in
        /// `crates/loom/src/web/reviews.rs`.
        #[operation(
    id = "reviews.comments.resolve",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
        pub struct Resolve;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
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

        impl Scoped for Input {
            fn scope_ref(&self) -> ScopeRef<'_> {
                ScopeRef::Global
            }
        }
    }

    pub mod update {
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
    }
}

pub mod create {
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
}

pub mod discard {
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
}

pub mod get {
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
}

pub mod list {
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
}

pub mod retarget {
    use super::prelude::*;

    /// Retarget a draft review's subject onto its current version — an
    /// artifact's latest revision, or the branch's current change-set — in one
    /// step, without touching anything else.
    ///
    /// Operator-only, and limited to the review's own creator — same reasoning
    /// as `reviews.comments.create`. Rejected once the review has left `draft`
    /// status. See `creator_review` in `crates/loom/src/web/reviews.rs`.
    #[operation(
    id = "reviews.retarget",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Retarget;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// The draft review to retarget.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision.
        pub expected_revision: i64,
    }

    pub type Output = ReviewDto;

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod retry_delivery {
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
}

pub mod submit {
    use super::prelude::*;

    /// Submit a review's draft, delivering its structured feedback into the
    /// reviewed session's own conversation.
    ///
    /// Operator-only, same reasoning as `reviews.comments.create` — only the
    /// review's creator may submit it. See `creator_review` in
    /// `crates/loom/src/web/reviews.rs`.
    #[operation(
    id = "reviews.submit",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
)]
    pub struct Submit;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
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

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

pub mod update {
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
}

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <discard::Discard as Operation>::SPEC,
    <retarget::Retarget as Operation>::SPEC,
    <list::List as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <comments::create::Create as Operation>::SPEC,
    <comments::update::Update as Operation>::SPEC,
    <comments::delete::Delete as Operation>::SPEC,
    <comments::resolve::Resolve as Operation>::SPEC,
    <submit::Submit as Operation>::SPEC,
    <retry_delivery::RetryDelivery as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "reviews",
        label: "Reviews",
        operations: OPERATIONS,
    }
}
