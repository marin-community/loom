use super::prelude::*;

/// Every registered runtime setting and its effective value.
///
/// `SessionSelf` because an agent may read the configuration it runs under —
/// `GET /settings` was reachable by a session credential before this was a
/// declaration. Writing one is `settings.patch`, which is `Admin`. The grant is
/// the session read grant: there is no narrower capability a session can hold,
/// and minting one nothing issues would deny the read outright.
#[operation(
    id = "settings.get",
    actor = SessionSelf,
    scope = Global,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    cli = "settings get",
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = SettingsEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
