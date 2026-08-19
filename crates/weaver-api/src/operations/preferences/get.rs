use super::prelude::*;

/// This operator's personal UI preference overrides (terminal theme, font,
/// font size), each layered over its effective inherited value.
///
/// `actor = User`: `grant_allows` has never admitted a session credential to
/// `/preferences` (no `Grant::Session` arm matches it, and it is absent from
/// the fixed list of bare paths a session may `GET`), so `User` — which also
/// covers `Admin` — is the exact set the legacy route already allowed.
#[operation(
    id = "preferences.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Get;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

pub type Output = UserPreferencesEnvelope;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
