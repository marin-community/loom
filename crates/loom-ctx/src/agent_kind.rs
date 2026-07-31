//! Provider-neutral agent and launch constants shared by the storage and
//! protocol layers.

/// The protected compatibility profile selected when no profile is named.
pub const DEFAULT_PROFILE: &str = "default";

/// Codex's workspace-write mode, with approvals owned by Loom.
pub const CODEX_AGENT_MODE: &str = "agent";

/// The permission posture every ACP session boots in when none is requested.
///
/// Claude's `auto` mode runs a background classifier and escalates risky calls.
/// Codex maps this posture to [`CODEX_AGENT_MODE`] plus Loom-owned approval.
pub const DEFAULT_ACP_MODE: &str = weaver_core::config::DEFAULT_AGENT_MODE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAgentKind {
    Claude,
    Codex,
}

impl BuiltinAgentKind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// Whether `kind` names one of the code-shipped runtimes.
pub fn is_builtin_agent_kind(kind: &str) -> bool {
    BuiltinAgentKind::parse(kind).is_some()
}

/// Whether an ACP mode asks Loom to auto-answer one-shot permission requests.
///
/// Claude's `bypassPermissions` and Codex's `agent-full-access` are explicit
/// no-prompt postures. Codex's ordinary [`CODEX_AGENT_MODE`] also routes
/// one-shot approval requests through Loom.
pub fn auto_approves_permissions(mode: &str) -> bool {
    matches!(
        mode.trim(),
        "bypassPermissions" | "agent-full-access" | CODEX_AGENT_MODE
    )
}
