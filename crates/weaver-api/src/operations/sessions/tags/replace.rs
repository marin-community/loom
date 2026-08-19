use super::prelude::*;

/// Atomically replace one author's complete tag set on a session.
///
/// The watch-safe counterpart to `sessions.tags.set`: rows still authored by
/// `by` are replaced in one transaction, so a stale round cannot delete a key
/// another actor took over after its fleet snapshot was taken. Doing this as a
/// diff of per-key calls is not the same operation — it loses exactly the
/// atomicity the watches depend on, which is why this is declared rather than
/// left to the caller to approximate.
///
/// `clear` names exact `(key, value)` pairs to drop as part of the same
/// transaction, so a real status can replace a lifecycle mark such as
/// `idle: idle` without the caller issuing a key-only delete that would also
/// remove someone else's newer value.
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
