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
    OpenCode,
    CursorAgent,
}

impl BuiltinAgentKind {
    /// Every builtin kind, in picker order. The single list a new harness is
    /// added to; downstream registries (`builtin_metadata`, the custom-agent
    /// reserved names, …) derive from it, and the per-kind `match`es are
    /// exhaustive so the compiler flags any that a new variant misses.
    pub const ALL: [BuiltinAgentKind; 4] =
        [Self::Claude, Self::Codex, Self::OpenCode, Self::CursorAgent];

    /// The wire id — what a session's `agent_kind` column and the picker store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::CursorAgent => "cursor-agent",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == kind)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_all_entry_round_trips_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for kind in BuiltinAgentKind::ALL {
            assert!(seen.insert(kind.as_str()), "duplicate in ALL: {kind:?}");
            assert_eq!(BuiltinAgentKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(BuiltinAgentKind::parse("nope"), None);
    }
}
