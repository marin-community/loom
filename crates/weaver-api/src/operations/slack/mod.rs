//! Slack integration status.
//!
//! Read-only, server-wide connection health for the Settings → Slack panel:
//! which credentials are set, whether `auth.test` resolves a live bot
//! identity, the configured access boundary, and the Socket Mode
//! supervisor's live health. This is a separate bundle because status is
//! server-wide (no branch), whereas the Slack write operation is
//! `branches.slack.reply`, which is branch-specific.

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
