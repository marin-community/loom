use super::prelude::*;

/// Atomically replace one author's complete tag set on a session.
///
/// All rows authored by `by` are replaced in a single transaction, ensuring
/// that a stale update cannot delete a key another actor took over after the
/// fleet snapshot. This atomic guarantee is required for the watch system to
/// avoid race conditions.
///
/// `clear` names exact `(key, value)` pairs to drop in the same transaction,
/// so a real status can replace a lifecycle mark (e.g., `idle: idle`) without
/// removing someone else's newer value.
#[operation(
    id = "sessions.tags.replace",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions tags replace",
)]
pub struct Replace;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The complete tag set this author now asserts.
    #[operand(json, default = Vec::new())]
    pub tags: Vec<TagInput>,
    /// Exact `(key, value)` pairs to clear in the same transaction.
    #[operand(json, default = Vec::new())]
    pub clear: Vec<TagMatch>,
    /// The author whose existing tag set is replaced. Defaults to `manual`.
    pub by: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = SessionView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
