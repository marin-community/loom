//! Repository preparation, provisioning, metadata assistance, and handoff.

pub mod handoff;
pub mod metadata_assist;
pub mod provision;
pub mod setup;

pub use loom_forge::AppState;
pub use loom_forge::{
    acp, agent, agent_env, agent_kind, auth, automation, backend, changes, channels, chat, chatlog,
    client_context, ctx, custom_agents, custom_mcp, db, envfile, github, github_app,
    github_manifest, github_trigger, history, ide, launch, launch_gate, lifecycle, links, logs,
    loom_config, mcp, paths, profile, profile_data, repo, repo_env, review_inbox, runner, runs,
    runtime, scratch, session, session_layout, session_manager, shell, status, terminal,
    user_token, Ctx, EditorState,
};

pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{config, events, git};
