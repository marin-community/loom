//! Launch, authentication, automation, and custom-MCP policy.

pub mod auth;
pub mod automation;
pub mod custom_mcp;
pub mod db;
pub mod profile;
pub mod user_token;

pub use loom_agent::Ctx;
pub use loom_agent::{
    acp, agent, agent_env, agent_kind, backend, changes, channels, chat, chatlog, client_context,
    ctx, custom_agents, envfile, history, launch_gate, links, logs, loom_config, mcp, paths,
    profile_data, repo_env, review_inbox, runner, runs, scratch, session, session_layout,
    slack_routes, status,
};

pub(crate) use weaver_core::config;
