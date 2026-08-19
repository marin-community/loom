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
pub mod connection_status {
    use super::prelude::*;

    /// Slack integration status: which credentials are set, whether `auth.test`
    /// resolves a live bot identity, the configured access boundary, and the
    /// Socket Mode supervisor's live health.
    #[operation(
    id = "slack.connection_status",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
    pub struct ConnectionStatus;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    /// The identity `auth.test` resolves, when a bot token is configured.
    /// `error` is set instead of the rest when the probe itself fails.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct SlackIdentityView {
        pub user_id: Option<String>,
        pub team_id: Option<String>,
        /// `"bot"` or `"user"`, depending on which kind of token is configured.
        pub token_kind: Option<String>,
        pub error: Option<String>,
    }

    /// Who may launch a session from Slack: the whole workspace, or a listed set
    /// of user ids (`users` is empty for the workspace-wide mode).
    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct SlackAccessView {
        pub mode: String,
        pub users: Vec<String>,
    }

    /// What the Socket Mode supervisor has seen, for the Connections pane and the
    /// logs.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct SlackSocketView {
        pub state: String,
        pub app_id: Option<String>,
        pub connected_at: Option<String>,
        pub last_error: Option<String>,
        pub last_event_at: Option<String>,
        pub events_received: u64,
        pub sessions_launched: u64,
        pub followups_routed: u64,
        pub last_skip: Option<String>,
        pub last_skip_at: Option<String>,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct Output {
        pub enabled: bool,
        pub app_token_set: bool,
        pub bot_token_set: bool,
        pub configured: bool,
        /// `None` when no bot credential is configured at all.
        pub identity: Option<SlackIdentityView>,
        pub access: SlackAccessView,
        pub default_repo: String,
        pub socket: SlackSocketView,
    }

    impl Scoped for Input {
        fn scope_ref(&self) -> ScopeRef<'_> {
            ScopeRef::Global
        }
    }
}

static OPERATIONS: &[&OperationSpec] = &[<connection_status::ConnectionStatus as Operation>::SPEC];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "slack",
        label: "Slack",
        operations: OPERATIONS,
    }
}
