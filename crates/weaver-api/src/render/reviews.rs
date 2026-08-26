//! Text rendering for review operations.
//!
//! A renderer is a pure function of the operation's `Output`, so these say what
//! the review now is rather than naming the comment or revision the caller just
//! passed in — `reviews.comments.delete` answers with the whole review, and the
//! deleted comment is precisely what it no longer contains.

use crate::dto::ReviewDto;
use crate::operations::reviews;
use crate::operations::{NoView, Render};

impl Render for reviews::comments::delete::Op {
    fn text(output: &ReviewDto, _: &NoView) -> String {
        format!(
            "review {} · draft revision {} · {} comments",
            output.id,
            output.draft_revision,
            output.comments.len()
        )
    }
}

impl Render for reviews::discard::Op {
    fn text(output: &reviews::discard::Output, _: &NoView) -> String {
        if output.discarded {
            "review discarded".to_string()
        } else {
            "review was not discarded".to_string()
        }
    }
}

impl Render for reviews::retarget::Op {
    fn text(output: &ReviewDto, _: &NoView) -> String {
        format!(
            "review {} targets {} version {} · draft revision {}",
            output.id, output.subject.key, output.subject.version, output.draft_revision
        )
    }
}

impl Render for reviews::retry_delivery::Op {
    fn text(output: &ReviewDto, _: &NoView) -> String {
        format!("review {} · delivery {}", output.id, output.delivery_state)
    }
}
