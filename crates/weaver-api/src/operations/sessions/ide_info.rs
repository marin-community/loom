use super::prelude::*;

/// Whether the embedded editor (code-server) is enabled and runnable on this
/// host, so a client can decide whether to offer it.
///
/// Host-level configuration; no session needs to be named.
#[operation(
    id = "sessions.ide_info",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "sessions ide-info",
)]
pub struct IdeInfo;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = SessionIdeInfoView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
