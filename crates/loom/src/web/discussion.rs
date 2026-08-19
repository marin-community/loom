use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use weaver_api::{AnchorDto, CommentDto, NewCommentBody, NewThreadBody, ThreadDto};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch::Branch;
use weaver_core::discussion;

use crate::events;

use super::{require_branch, require_session};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Discussion — resolvable, stand-off comment threads anchored to a quoted span
// of an artifact. See `weaver_core::discussion` and docs/artifacts.md. Name
// resolution mirrors the artifact endpoints (branch-scoped, then
// repo-shared); API-originated threads/comments are authored `"user"`. The
// branch-scoped twins below serve `loom artifacts comment/resolve/threads`,
// which — like the other worktree-facing Loom commands — need no live session.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Branch-scoped discussion — the twin of the session-scoped routes above, for
// a `loom artifacts comment/resolve/threads` target with no live session
// required, matching the branch-scoped artifact routes in `artifacts.rs`.
// ---------------------------------------------------------------------------
