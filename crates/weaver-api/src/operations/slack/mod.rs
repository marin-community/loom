//! Slack integration status.
//!
//! Read-only, server-wide connection health for the Settings → Slack panel:
//! which credentials are set, whether `auth.test` resolves a live bot
//! identity, the configured access boundary, and the Socket Mode
//! supervisor's live health. The one Slack *write* — a branch's own session
//! replying to a thread it owns — is `branches.slack.reply`, already
//! registered in the `branches` bundle; this is a separate bundle rather than
//! joining it because status has no branch in sight at all (the legacy
//! `GET /slack/status` route takes no path parameter), unlike `reply`, whose
//! whole shape is "this branch, this thread."

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod connection_status;

static OPERATIONS: &[&OperationSpec] = &[<connection_status::ConnectionStatus as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "slack",
        label: "Slack",
        operations: OPERATIONS,
    }
}
