//! Agent runtimes, ACP, and trusted MCP mechanisms.

pub mod acp;
pub mod agent;
pub mod custom_agents;
pub mod mcp;

pub use loom_store::profile_data as profile;
pub use loom_store::Ctx;
pub use loom_store::{
    agent_env, agent_kind, backend, changes, channels, chat, chatlog, client_context, ctx, db,
    envfile, history, launch_gate, links, logs, loom_config, paths, profile_data, repo_env,
    review_inbox, runner, runs, scratch, session, session_layout, status,
};

pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::events;
