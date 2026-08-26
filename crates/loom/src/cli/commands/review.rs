//! The `loom review` subcommands the registry cannot declare.
//!
//! `show`, `discard`, `retarget`, `delete-comment` and `retry` are declared on
//! their operations in `weaver_api::operations::reviews` and merge in beside
//! these. What is left does something one declaration cannot: `ls` pins
//! `subject_kind` to `artifact` and names a session that `reviews.list` takes
//! from context; `add` and `overall` each run two operations in sequence;
//! `add`, `edit` and `reanchor` build a `ReviewAnchorDto`, or a joined
//! multi-word body, out of flags; `submit` updates the summary first when one
//! is given; and `resolve`/`reopen` are two spellings of one operation that
//! differ only in the `resolved` they send.

use crate::client;
use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use weaver_api::operations::reviews;
use weaver_api::{
    ArtifactTextAnchorDto, ReviewAnchorDto, ReviewAnchorKindDto, ReviewSubjectKindDto,
};

#[derive(Subcommand)]
pub enum ReviewCmd {
    /// List reviews for one artifact in a session.
    Ls { session: String, artifact: String },
    /// Add a pending comment, creating the caller's draft when needed.
    Add {
        session: String,
        artifact: String,
        #[arg(long)]
        rev: i64,
        #[arg(long)]
        quote: String,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "")]
        suffix: String,
        #[arg(long)]
        block: Option<i64>,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Edit a pending comment body.
    Edit {
        review_id: i64,
        comment_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Move a pending comment to a new text/block anchor and revision.
    Reanchor {
        review_id: i64,
        comment_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(long)]
        rev: i64,
        #[arg(long)]
        quote: String,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "")]
        suffix: String,
        #[arg(long)]
        block: Option<i64>,
    },
    /// Create or update an overall-note-only draft.
    Overall {
        session: String,
        artifact: String,
        #[arg(long)]
        rev: i64,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// Resolve one submitted review comment.
    Resolve { review_id: i64, comment_id: i64 },
    /// Reopen one resolved review comment.
    Reopen { review_id: i64, comment_id: i64 },
    /// Submit the immutable review and enqueue one structured conversation message.
    Submit {
        review_id: i64,
        /// Draft revision shown by `loom review ls` or the previous mutation.
        #[arg(long)]
        revision: i64,
        #[arg(long, default_value = "")]
        summary: String,
        /// Intentionally submit anchors from an older artifact revision.
        #[arg(long)]
        acknowledge_outdated: bool,
    },
}

pub async fn run_review(cmd: ReviewCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        ReviewCmd::Ls { session, artifact } => {
            let reviews = client
                .invoke::<reviews::list::Op>(&reviews::list::Input {
                    subject_kind: "artifact".parse().map_err(anyhow::Error::msg)?,
                    subject_key: artifact.to_string(),
                    session: session.to_string(),
                })
                .await?;
            if reviews.is_empty() {
                println!("(no reviews)");
                return Ok(());
            }
            for review in reviews {
                let stale = if review.outdated { " stale" } else { "" };
                println!(
                    "#{} {} · draft rev {} · {} comments · {}{}",
                    review.id,
                    review.status,
                    review.draft_revision,
                    review.comments.len(),
                    review.delivery_state,
                    stale
                );
                for comment in review.comments {
                    println!(
                        "  {}  rev {}  {}",
                        comment.id,
                        comment.subject_version,
                        comment.body.replace('\n', " ")
                    );
                }
            }
            Ok(())
        }
        ReviewCmd::Add {
            session,
            artifact,
            rev,
            quote,
            prefix,
            suffix,
            block,
            body,
        } => {
            let body = body.join(" ").trim().to_string();
            if body.is_empty() {
                bail!("a comment body is required");
            }
            let draft = client
                .invoke::<reviews::create::Op>(&reviews::create::Input {
                    session: session.to_string(),
                    subject_kind: ReviewSubjectKindDto::Artifact,
                    subject_key: artifact.clone(),
                    subject_version: rev.to_string(),
                })
                .await?;
            let comment = client
                .invoke::<reviews::comments::create::Op>(&reviews::comments::create::Input {
                    id: draft.id,
                    expected_revision: draft.draft_revision,
                    subject_version: rev.to_string(),
                    anchor_kind: ReviewAnchorKindDto::Text,
                    anchor: (ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                        quote,
                        prefix,
                        suffix,
                        block_index: block,
                    }))
                    .clone(),
                    body: body.clone(),
                })
                .await?;
            let comment_id = comment
                .comments
                .last()
                .map(|comment| comment.id)
                .ok_or_else(|| anyhow!("server returned a draft without the added comment"))?;
            println!(
                "draft review #{} · revision {} · comment {}",
                draft.id, comment.draft_revision, comment_id
            );
            Ok(())
        }
        ReviewCmd::Edit {
            review_id,
            comment_id,
            revision,
            body,
        } => {
            let body = body.join(" ").trim().to_string();
            if body.is_empty() {
                bail!("a comment body is required");
            }
            let comment = client
                .invoke::<reviews::comments::update::Op>(&reviews::comments::update::Input {
                    id: review_id,
                    comment_id,
                    expected_revision: revision,
                    body: (Some(body)).clone(),
                    ..Default::default()
                })
                .await?;
            println!(
                "updated comment {comment_id} · draft revision {}",
                comment.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Reanchor {
            review_id,
            comment_id,
            revision,
            rev,
            quote,
            prefix,
            suffix,
            block,
        } => {
            let comment = client
                .invoke::<reviews::comments::update::Op>(&reviews::comments::update::Input {
                    id: review_id,
                    comment_id,
                    expected_revision: revision,
                    body: None.clone(),
                    subject_version: (Some(rev.to_string())).clone(),
                    anchor_kind: (Some(ReviewAnchorKindDto::Text)),
                    anchor: (Some(ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                        quote,
                        prefix,
                        suffix,
                        block_index: block,
                    })))
                    .clone(),
                })
                .await?;
            println!(
                "re-anchored comment {comment_id} to revision {rev} · draft revision {}",
                comment.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Overall {
            session,
            artifact,
            rev,
            body,
        } => {
            let summary = body.join(" ").trim().to_string();
            if summary.is_empty() {
                bail!("an overall note is required");
            }
            let draft = client
                .invoke::<reviews::create::Op>(&reviews::create::Input {
                    session: session.to_string(),
                    subject_kind: ReviewSubjectKindDto::Artifact,
                    subject_key: artifact.clone(),
                    subject_version: rev.to_string(),
                })
                .await?;
            let draft = client
                .invoke::<reviews::update::Op>(&reviews::update::Input {
                    id: draft.id,
                    expected_revision: draft.draft_revision,
                    summary: (Some(summary)).clone(),
                    subject_version: None.clone(),
                })
                .await?;
            println!(
                "draft review #{} · revision {} · overall note saved",
                draft.id, draft.draft_revision
            );
            Ok(())
        }
        ReviewCmd::Resolve {
            review_id,
            comment_id,
        } => {
            let comment = client
                .invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
                    id: review_id,
                    comment_id,
                    resolved: true,
                })
                .await?;
            println!("resolved comment {}", comment.id);
            Ok(())
        }
        ReviewCmd::Reopen {
            review_id,
            comment_id,
        } => {
            let comment = client
                .invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
                    id: review_id,
                    comment_id,
                    resolved: false,
                })
                .await?;
            println!("reopened comment {}", comment.id);
            Ok(())
        }
        ReviewCmd::Submit {
            review_id,
            revision,
            summary,
            acknowledge_outdated,
        } => {
            let revision = if summary.is_empty() {
                revision
            } else {
                client
                    .invoke::<reviews::update::Op>(&reviews::update::Input {
                        id: review_id,
                        expected_revision: revision,
                        summary: (Some(summary)).clone(),
                        subject_version: None.clone(),
                    })
                    .await?
                    .draft_revision
            };
            let review = client
                .invoke::<reviews::submit::Op>(&reviews::submit::Input {
                    id: review_id,
                    expected_revision: revision,
                    acknowledge_outdated,
                })
                .await?;
            println!(
                "submitted review {} · delivery {}",
                review.id, review.delivery_state
            );
            Ok(())
        }
    }
}
