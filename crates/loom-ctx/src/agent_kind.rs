//! Provider-neutral agent and launch constants shared by the storage and
//! protocol layers. Harness-specific behaviour lives in `loom_agent::harnesses`.

/// The protected compatibility profile selected when no profile is named.
pub const DEFAULT_PROFILE: &str = "default";

/// The permission posture every ACP session boots in when none is requested.
///
/// Claude's `auto` mode runs a background classifier and escalates risky calls.
/// Codex maps this posture to its `agent` mode plus Loom-owned approval.
pub const DEFAULT_ACP_MODE: &str = weaver_core::config::DEFAULT_AGENT_MODE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAgentKind {
    Claude,
    Codex,
    OpenCode,
    CursorAgent,
}

impl BuiltinAgentKind {
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
