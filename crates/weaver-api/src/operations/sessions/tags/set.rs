use super::prelude::*;

/// Set one free-form session tag.
#[operation(
    id = "sessions.tags.set",
    actor = SessionSelf,
    scope = Session,
    risk = Write,
    grants = ["loom/sessions/write@v1"],
    cli = "sessions tags set",
)]
pub struct Set;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The tag key.
    #[operand(positional)]
    pub key: String,
    /// The tag value.
    #[operand(positional)]
    pub value: String,
    /// One-line reason accompanying the tag.
    #[operand(default = String::new())]
    pub note: String,
    /// Who is setting it (a watch name, or blank for `manual`).
    pub by: Option<String>,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}

pub type Output = BranchView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Session(&self.session)
    }
}
