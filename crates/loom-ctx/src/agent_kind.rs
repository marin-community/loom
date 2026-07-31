//! Provider-neutral agent and launch constants shared by the storage and
//! protocol layers.

/// The protected compatibility profile selected when no profile is named.
pub const DEFAULT_PROFILE: &str = "default";

/// The permission posture every ACP session boots in when none is requested.
pub const DEFAULT_ACP_MODE: &str = weaver_core::config::DEFAULT_AGENT_MODE;

/// Whether `kind` names one of the code-shipped runtimes.
pub fn is_builtin_agent_kind(kind: &str) -> bool {
    matches!(kind, "claude" | "codex")
}

/// Whether an ACP mode asks Loom to auto-answer one-shot permission requests.
pub fn auto_approves_permissions(mode: &str) -> bool {
    matches!(
        mode.trim(),
        "bypassPermissions" | "agent-full-access" | "agent"
    )
}
