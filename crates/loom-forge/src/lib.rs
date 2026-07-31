//! GitHub integration, repository registry, and session runtime lifecycle.

pub mod github;
pub mod github_app;
pub mod github_manifest;
pub mod github_trigger;
pub mod lifecycle;
pub mod repo;
pub mod runtime;
pub mod user_token;

pub use loom_editor::Ctx;
pub use loom_editor::{
    acp, agent, agent_env, agent_kind, auth, automation, backend, changes, channels, chat, chatlog,
    client_context, ctx, custom_agents, custom_mcp, db, envfile, history, ide, launch, launch_gate,
    links, logs, loom_config, mcp, paths, profile, profile_data, repo_env, review_inbox, runner,
    runs, scratch, session, session_layout, session_manager, shell, status, terminal, EditorState,
};

/// Shared process state consumed by runtime services and the REST adapter.
#[derive(Clone)]
pub struct AppState {
    pub ctx: Ctx,
    pub ide: std::sync::Arc<ide::IdeManager>,
    pub trigger: std::sync::Arc<github_trigger::GithubTrigger>,
    pub acp: acp::AcpRegistry,
    pub launch_gate: launch_gate::RepoLaunchGate,
}

impl AppState {
    pub fn acp_ctx(&self) -> acp::AcpCtx {
        acp::AcpCtx {
            ctx: self.ctx.clone(),
            acp: self.acp.clone(),
        }
    }

    pub fn editor_state(&self) -> EditorState {
        EditorState {
            ctx: self.ctx.clone(),
            ide: self.ide.clone(),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = Ctx;
    fn deref(&self) -> &Ctx {
        &self.ctx
    }
}

impl axum::extract::FromRef<AppState> for EditorState {
    fn from_ref(state: &AppState) -> Self {
        state.editor_state()
    }
}

pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{branch, config, events, git};
