use super::prelude::*;

/// Subscribe to one session's live event feed.
///
/// Session is provided as an operand (not a path segment) to follow the standard
/// route pattern.
///
/// `io = Stream` changes exactly one thing: the response encoding, so a custom
/// handler serves it instead of the JSON dispatcher. The actor policy, the
/// grants, and the resource scope are read from this declaration by that
/// handler — see `loom::web::encodings`.
#[operation(
    id = "sessions.events.stream",
    actor = SessionSelf,
    scope = Session,
    risk = Read,
    grants = ["loom/sessions/read@v1"],
    io = Stream,
)]
pub struct Stream;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A visible session id. Omit for this session.
    //
    // `serde(default)` because a stream's operands arrive in the query string,
    // which is extracted before the dispatcher's default-filling step can run.
    // `streams_take_every_operand_from_the_query_string` pins this for all of
    // them.
    #[serde(default)]
    #[operand(context)]
    pub session: String,
}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
