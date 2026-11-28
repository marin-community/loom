//! The `opencode` builtin: Opencode's TUI as the terminal fallback and its
//! built-in ACP server (`opencode acp`). Credentials ride the per-profile
//! `OPENCODE_CONFIG_CONTENT` variable; the shared credential store is
//! deploy-provisioned (see `catalog::provision`).

use serde_json::Value;
use weaver_core::BoxFut;

use super::{binary_on_path, join_args, model_flag, spaced, AcpContract, BuiltinHarness};
use crate::agent::{
    sh_single_quote, sh_single_quote_path, start_terminal, AgentLaunchContext, AgentMetadata,
    AgentType, LaunchMode,
};
use crate::db::Db;
use loom_ctx::agent_kind::BuiltinAgentKind;

pub static HARNESS: OpenCodeAgentType = OpenCodeAgentType;

pub struct OpenCodeAgentType;

impl AgentType for OpenCodeAgentType {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            kind: "opencode".to_string(),
            label: "Opencode".to_string(),
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
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Fresh)).await
        })
    }

    fn adopt<'a>(&'a self, ctx: AgentLaunchContext<'a>) -> crate::agent::AgentFuture<'a> {
        Box::pin(async move {
            start_terminal(&ctx, "opencode", &opencode_command(&ctx, LaunchMode::Adopt)).await
        })
    }
}

impl BuiltinHarness for OpenCodeAgentType {
    fn kind(&self) -> BuiltinAgentKind {
        BuiltinAgentKind::OpenCode
    }

    fn credential_names(&self) -> &'static [&'static str] {
        // A credential rotation is applied by the boot import and by the
        // per-pass list/prune; the payload itself is stripped from the
        // environment before fingerprinting, so it cannot be hashed here.
        &["OPENCODE_CONFIG_CONTENT"]
    }

    fn available(&self) -> BoxFut<'static, bool> {
        Box::pin(binary_on_path("opencode"))
    }

    fn acp_command(&self, db: Db) -> BoxFut<'static, String> {
        Box::pin(async move {
            super::acp_cmd_override(&db, "WEAVER_OPENCODE_ACP_CMD", "acp.opencode_cmd")
                .await
                .unwrap_or_else(|| "opencode acp".to_string())
        })
    }

    fn acp_mode_for(&self, mode: &str) -> Option<&'static str> {
        let mode = mode.trim();
        // Opencode ships two agents: `build` runs tools under its own
        // permission prompts, `plan` is read-only. There is no
        // accepted-edits or bypass posture, so neither is offered rather than
        // silently running with different prompts than the name implies.
        Some(match mode {
            "default" | "auto" | "" | "build" => "build",
            "plan" => "plan",
            _ => return None,
        })
    }

    fn auto_approves(&self, _mode: &str) -> bool {
        // Opencode prompts per its own permission config in both of its modes.
        false
    }

    fn seeds_primer_as_goal(&self) -> bool {
        true
    }

    fn acp_meta(&self, _contract: &AcpContract<'_>) -> Option<Value> {
        None
    }
}

// -- terminal launch --------------------------------------------------------

/// The Opencode TUI is the terminal fallback for the ACP builtin: `--continue`
/// resumes in an existing worktree, a fresh run preloads the goal via
/// `--prompt`.
pub(crate) fn opencode_command(ctx: &AgentLaunchContext<'_>, mode: LaunchMode) -> String {
    let args = spaced(join_args([
        model_flag(ctx.model).map(|m| format!("--model {}", sh_single_quote(m)))
    ]));
    match (mode, ctx.goal_file.or(ctx.primer_file)) {
        (LaunchMode::Adopt, _) => format!("opencode --continue{args}"),
        (LaunchMode::Fresh, Some(f)) => {
            format!(
                "opencode{args} --prompt \"$(cat {})\"",
                sh_single_quote_path(f)
            )
        }
        (LaunchMode::Fresh, None) => format!("opencode{args}"),
    }
}
