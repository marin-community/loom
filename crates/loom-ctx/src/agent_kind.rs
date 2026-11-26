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
///
/// Call [`auto_approves_permissions_for`] where the harness is known: Cursor
/// names its *ordinary* mode `agent` too, so the bare id means something
/// different there.
pub fn auto_approves_permissions(mode: &str) -> bool {
    matches!(
        mode.trim(),
        "bypassPermissions" | "agent-full-access" | CODEX_AGENT_MODE
    )
}

/// [`auto_approves_permissions`] for a specific harness, so one harness's
/// ordinary mode id cannot be read as another's no-prompt posture.
pub fn auto_approves_permissions_for(kind: Option<BuiltinAgentKind>, mode: &str) -> bool {
    match kind {
        // Cursor's `agent` mode prompts for every tool call; only Claude's
        // bypass posture is a no-prompt mode in that vocabulary.
        Some(BuiltinAgentKind::CursorAgent) => mode.trim() == "bypassPermissions",
        // Opencode prompts per its own permission config in both of its modes.
        Some(BuiltinAgentKind::OpenCode) => false,
        _ => auto_approves_permissions(mode),
    }
}

/// The ACP mode id each harness advertises for one of Loom's launch postures.
///
/// Loom's mode vocabulary is Claude's. Codex, Opencode and Cursor each
/// advertise their own ids, so a launch has to translate rather than pass a
/// claude-flavoured id through. A harness's own advertised id maps to itself:
/// a live session persists the adapter id in `Session.current_mode`, and a
/// respawn replays that value back through this mapping, so it must accept
/// what the adapter actually advertises — for every harness, not just the ones
/// whose ids happen to match Loom's.
///
/// `None` means the harness has no equivalent and the launch must not silently
/// fall back to a different permission posture.
pub fn acp_mode_for(kind: BuiltinAgentKind, mode: &str) -> Option<&'static str> {
    let mode = mode.trim();
    match kind {
        BuiltinAgentKind::Codex => Some(match mode {
            "bypassPermissions" => "agent-full-access",
            // codex-acp has no distinct auto-approve mode. `agent` supplies the
            // workspace sandbox; Loom answers its one-shot approval requests.
            "acceptEdits" | "default" | "auto" | "" => CODEX_AGENT_MODE,
            "plan" => "read-only",
            other => return codex_passthrough(other),
        }),
        // Opencode ships two agents: `build` runs tools under its own
        // permission prompts, `plan` is read-only. There is no
        // accepted-edits or bypass posture, so neither is offered rather than
        // silently running with different prompts than the name implies.
        BuiltinAgentKind::OpenCode => Some(match mode {
            "default" | "auto" | "" | "build" => "build",
            "plan" => "plan",
            _ => return None,
        }),
        // Cursor advertises `agent`, `plan` and `ask`; its `agent` mode always
        // routes tool approval through the client, so it has no bypass or
        // accepted-edits posture either.
        BuiltinAgentKind::CursorAgent => Some(match mode {
            "default" | "auto" | "" | "agent" => "agent",
            "plan" => "plan",
            "ask" => "ask",
            _ => return None,
        }),
        BuiltinAgentKind::Claude => Some(match mode {
            "" => DEFAULT_ACP_MODE,
            "auto" => "auto",
            "default" => "default",
            "acceptEdits" => "acceptEdits",
            "plan" => "plan",
            "bypassPermissions" => "bypassPermissions",
            _ => return None,
        }),
    }
}

impl BuiltinAgentKind {
    /// The Loom modes this harness can honour, for the picker and for profile
    /// validation. Every mode here has an [`acp_mode_for`] mapping, so choosing
    /// one cannot silently leave the harness on a different posture.
    pub fn acp_modes(self) -> Vec<&'static str> {
        [
            "auto",
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
        ]
        .into_iter()
        .filter(|mode| acp_mode_for(self, mode).is_some())
        .collect()
    }
}

/// Loom's postures, in the order a reverse lookup prefers.
const LOOM_POSTURES: [&str; 5] = [
    "default",
    "auto",
    "acceptEdits",
    "plan",
    "bypassPermissions",
];

/// The Loom posture a stored mode id stands for.
///
/// A live session persists its adapter's own id; surfaces that must speak
/// Loom's vocabulary — a handoff to a different harness, which translates the
/// mode fresh for the target — use this to get back to a posture. An id with
/// no Loom equivalent (Cursor's `ask`) has no answer, and the caller must not
/// silently substitute one.
pub fn loom_mode_for(kind: BuiltinAgentKind, mode: &str) -> Option<&'static str> {
    let mode = mode.trim();
    LOOM_POSTURES
        .into_iter()
        .find(|posture| *posture == mode || acp_mode_for(kind, posture) == Some(mode))
}

/// Codex's own mode ids pass through so an operator can name one directly —
/// `agent` is the id a live session persists for its ordinary posture.
fn codex_passthrough(mode: &str) -> Option<&'static str> {
    match mode {
        "agent" => Some(CODEX_AGENT_MODE),
        "read-only" => Some("read-only"),
        "agent-full-access" => Some("agent-full-access"),
        _ => None,
    }
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

    /// A live session persists the adapter's mode id in `current_mode` and a
    /// respawn replays it through this mapping, so every id an adapter
    /// advertises must map to itself — not just the harnesses whose ids happen
    /// to match Loom's vocabulary.
    #[test]
    fn a_harness_replays_its_own_advertised_mode_ids() {
        let native: &[(BuiltinAgentKind, &[&str])] = &[
            (
                BuiltinAgentKind::Claude,
                &[
                    "auto",
                    "default",
                    "acceptEdits",
                    "plan",
                    "bypassPermissions",
                ],
            ),
            (
                BuiltinAgentKind::Codex,
                &["read-only", "agent", "agent-full-access"],
            ),
            (BuiltinAgentKind::OpenCode, &["build", "plan"]),
            (BuiltinAgentKind::CursorAgent, &["agent", "plan", "ask"]),
        ];
        for (kind, ids) in native {
            for id in *ids {
                assert_eq!(
                    acp_mode_for(*kind, id),
                    Some(*id),
                    "{} must replay its own '{id}'",
                    kind.as_str()
                );
            }
        }
    }

    #[test]
    fn a_posture_a_harness_cannot_express_is_still_rejected() {
        for kind in [BuiltinAgentKind::OpenCode, BuiltinAgentKind::CursorAgent] {
            assert_eq!(acp_mode_for(kind, "bypassPermissions"), None);
            assert_eq!(acp_mode_for(kind, "acceptEdits"), None);
            assert_eq!(acp_mode_for(kind, "not-a-mode"), None);
        }
    }
}
