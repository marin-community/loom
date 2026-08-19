use super::prelude::*;

/// Run a one-shot ACP prompt through a registered agent runtime and return its
/// text — the judgement-call primitive watch programs call.
///
/// `actor = User`: the legacy `POST /agent/oneshot` route has never admitted a
/// session credential (`grant_allows` in `crates/loom/src/web/auth.rs` has no
/// `Grant::Session` arm for `/agent/oneshot`; `user_grant_allows` does not
/// refuse it, so a plain signed-in `User` reaches it same as today).
///
/// `risk = ExternalWrite`: without a `profile` the prompt runs with no branch
/// or session sandbox and no automation-safe policy constraining it — the
/// same blast radius as `shell.terminal`, just LLM-issued instructions rather
/// than operator-typed ones.
#[operation(
    id = "agents.oneshot",
    actor = User,
    scope = Global,
    risk = ExternalWrite,
    grants = [],
)]
pub struct Oneshot;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The prompt to run.
    #[operand(positional)]
    pub prompt: String,
    /// Optional launch profile. When set, its runtime and policy are
    /// authoritative; model and effort remain optional per-call overrides.
    #[operand(default = String::new())]
    pub profile: String,
    /// Registered ACP runtime. Empty preserves the historical Claude default.
    #[operand(default = String::new())]
    pub agent: String,
    /// Model override advertised by the runtime; empty keeps its ACP default.
    #[operand(default = String::new())]
    pub model: String,
    /// Reasoning effort override advertised by the runtime; empty keeps its
    /// ACP default.
    #[operand(default = String::new())]
    pub effort: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    /// `null` when the adapter is absent or fails — callers degrade to their
    /// own deterministic fallback rather than seeing an error.
    pub output: Option<String>,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
