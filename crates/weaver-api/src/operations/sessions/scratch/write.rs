use super::prelude::*;

/// Write one Scratch file from a raw request body.
///
/// The only `io = Upload` operation, and the reason that variant exists: the
/// body is the file's bytes, so there is no JSON envelope to put operands in and
/// they arrive in the query string instead. Launch-time attachments take the
/// other road — `sessions.launch` carries them base64-encoded inside its JSON,
/// because there one request has to carry several files *and* the rest of the
/// launch configuration.
#[operation(
    id = "sessions.scratch.write",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    io = Upload,
)]
pub struct Write;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The file name to write, a single path component.
    //
    // `serde(default)` because an upload's operands arrive in the query string,
    // which axum extracts before the dispatcher's default-filling step runs. The
    // handler rejects an empty name.
    #[serde(default)]
    #[operand(default = String::new())]
    pub name: String,
    /// A visible session id. Omit for this session.
    #[serde(default)]
    #[operand(context)]
    pub session: String,
}

pub type Output = ScratchWriteResult;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
