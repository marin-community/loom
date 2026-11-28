//! Agent runtimes, ACP, and trusted MCP mechanisms.

pub mod acp;
pub mod agent;
pub mod catalog;
pub mod custom_agents;
pub mod harnesses;
pub mod mcp;
pub mod skills;

/// The agent-kind vocabulary: the ids every layer stores, plus the mode
/// mappings the builtin harness registry owns. The enum and the default
/// posture live in `loom_ctx` (the storage layer reads them); everything
/// harness-specific lives behind [`crate::harnesses`].
pub mod agent_kind {
    pub use crate::harnesses::codex::CODEX_AGENT_MODE;
    pub use crate::harnesses::{
        acp_mode_for, acp_modes, auto_approves_permissions, auto_approves_permissions_for,
        loom_mode_for,
    };
    pub use loom_ctx::agent_kind::{
        is_builtin_agent_kind, BuiltinAgentKind, DEFAULT_ACP_MODE, DEFAULT_PROFILE,
    };
}

pub use loom_ctx::env_policy;
pub use loom_store::profile_data as profile;
pub use loom_store::Ctx;
pub use loom_store::{
    agent_env, backend, changes, channels, chat, chatlog, client_context, conversation_inbox, ctx,
    db, envfile, history, launch_gate, links, logs, loom_config, paths, profile_data, repo_env,
    runner, runs, scratch, session, session_layout, slack_routes, status,
};

pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::events;
