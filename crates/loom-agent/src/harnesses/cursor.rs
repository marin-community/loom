//! The `cursor-agent` builtin: Cursor's CLI agent (`cursor-agent` or
//! `cursor agent`) over the terminal, and its built-in ACP server
//! (`cursor-agent acp`). Cursor opts into the parameterized model picker, so
//! its catalogue exposes per-model effort and fast options.

use std::time::Duration;

use serde_json::{json, Value};
use weaver_core::BoxFut;

use super::{binary_on_path, join_args, model_flag, spaced, AcpContract, BuiltinHarness};
use crate::agent::{
    sh_single_quote, sh_single_quote_path, start_terminal, AgentLaunchContext, AgentMetadata,
    AgentType, LaunchMode,
};
use crate::db::Db;
use loom_ctx::agent_kind::BuiltinAgentKind;

pub static HARNESS: CursorAgentType = CursorAgentType;

pub struct CursorAgentType;

impl AgentType for CursorAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "cursor-agent".to_string(),
            label: "Cursor".to_string(),
            // The picker reads models from the stored catalogue.
            models: Vec::new(),
            efforts: Vec::new(),
            accepts_raw_model: true,
            supports_hooks: false,
            builtin: true,
            supports_acp: true,
            protocol: "acp".to_string(),
            available: None,
            modes: self.acp_modes().into_iter().map(str::to_string).collect(),
        }
    }

    fn create<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            let bin = cursor_agent_bin().await.unwrap_or("cursor-agent");
            start_terminal(
                &ctx,
                "cursor-agent",
                &cursor_agent_command(bin, &ctx, LaunchMode::Fresh),
            )
            .await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            let bin = cursor_agent_bin().await.unwrap_or("cursor-agent");
            start_terminal(
                &ctx,
                "cursor-agent",
                &cursor_agent_command(bin, &ctx, LaunchMode::Adopt),
            )
            .await
        })
    }
}

impl BuiltinHarness for CursorAgentType {
    fn kind(&self) -> BuiltinAgentKind {
        BuiltinAgentKind::CursorAgent
    }

    fn credential_names(&self) -> &'static [&'static str] {
        &["CURSOR_API_KEY", "CURSOR_AUTH_TOKEN"]
    }

    /// Cursor opts into separate per-model options. Without this, the adapter
    /// bundles effort and fast into the model id and no selector appears.
    fn initialize_meta(&self) -> Option<Value> {
        Some(json!({ "parameterizedModelPicker": true }))
    }

    fn available(&self) -> BoxFut<'static, bool> {
        Box::pin(async move { cursor_agent_bin().await.is_some() })
    }

    fn acp_command(&self, db: Db) -> BoxFut<'static, String> {
        Box::pin(async move {
            if let Some(cmd) =
                super::acp_cmd_override(&db, "WEAVER_CURSOR_ACP_CMD", "acp.cursor_cmd").await
            {
                return cmd;
            }
            format!("{} acp", cursor_agent_bin().await.unwrap_or("cursor-agent"))
        })
    }

    fn acp_mode_for(&self, mode: &str) -> Option<&'static str> {
        let mode = mode.trim();
        // Cursor advertises `agent`, `plan` and `ask`; its `agent` mode always
        // routes tool approval through the client, so it has no bypass or
        // accepted-edits posture either.
        Some(match mode {
            "default" | "auto" | "" | "agent" => "agent",
            "plan" => "plan",
            "ask" => "ask",
            _ => return None,
        })
    }

    fn auto_approves(&self, mode: &str) -> bool {
        // Cursor's `agent` mode prompts for every tool call; only Claude's
        // bypass posture is a no-prompt mode in that vocabulary.
        mode.trim() == "bypassPermissions"
    }

    /// Cursor's adapter throttles back-to-back per-model probes.
    fn probe_pace(&self) -> Duration {
        Duration::from_millis(200)
    }

    fn seeds_primer_as_goal(&self) -> bool {
        true
    }

    fn acp_meta(&self, _contract: &AcpContract<'_>) -> Option<Value> {
        None
    }
}

// -- terminal launch --------------------------------------------------------

/// How to invoke Cursor's CLI agent: the Homebrew `cursor-agent` binary when
/// present, else the `cursor agent` subcommand. `None` when neither is on
/// `PATH`.
pub(super) async fn cursor_agent_bin() -> Option<&'static str> {
    if binary_on_path("cursor-agent").await {
        Some("cursor-agent")
    } else if binary_on_path("cursor").await {
        Some("cursor agent")
    } else {
        None
    }
}

/// Cursor's CLI as the terminal fallback: `--continue` resumes, a fresh run
/// takes the goal positionally. `--model` is passed through verbatim (Cursor
/// scopes effort inside the model id / bracket parameters).
pub(crate) fn cursor_agent_command(
    bin: &str,
    ctx: &AgentLaunchContext<'_>,
    mode: LaunchMode,
) -> String {
    let args = spaced(join_args([
        model_flag(ctx.model).map(|m| format!("--model {}", sh_single_quote(m)))
    ]));
    match (mode, ctx.goal_file.or(ctx.primer_file)) {
        (LaunchMode::Adopt, _) => format!("{bin} --continue{args}"),
        (LaunchMode::Fresh, Some(f)) => {
            format!("{bin}{args} \"$(cat {})\"", sh_single_quote_path(f))
        }
        (LaunchMode::Fresh, None) => format!("{bin}{args}"),
    }
}
