//! Live terminal and embedded-editor attachment.

pub mod ide;
pub mod terminal;

pub use loom_core::Ctx;
pub use loom_core::{
    acp, agent, agent_env, agent_kind, auth, automation, backend, changes, channels, chat, chatlog,
    client_context, ctx, custom_agents, custom_mcp, db, envfile, history, launch, launch_gate,
    links, logs, loom_config, mcp, paths, profile, profile_data, repo_env, review_inbox, runner,
    runs, scratch, session, session_layout, session_manager, shell, status,
};

/// The state slice editor and terminal handlers actually consume.
#[derive(Clone)]
pub struct EditorState {
    pub ctx: Ctx,
    pub ide: std::sync::Arc<ide::IdeManager>,
}

impl std::ops::Deref for EditorState {
    type Target = Ctx;
    fn deref(&self) -> &Ctx {
        &self.ctx
    }
}

pub(crate) use weaver_core::config;
