use super::prelude::*;

/// Create a named session-launch profile.
#[operation(
    id = "profiles.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles create",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile's name.
    #[operand(positional)]
    pub name: String,
    #[operand(default = String::new())]
    pub description: String,
    /// Agent runtime this profile launches (e.g. `claude`, `codex`).
    pub agent_kind: String,
    /// Blank uses the runtime's own default.
    #[operand(default = String::new())]
    pub model: String,
    /// Blank uses the runtime's own default.
    #[operand(default = String::new())]
    pub effort: String,
    /// Blank uses the runtime's own default.
    #[operand(default = String::new())]
    pub protocol: String,
    /// Blank uses the runtime's own default.
    #[operand(default = String::new())]
    pub mode: String,
    #[operand(default = String::from("interactive"))]
    pub class: String,
    #[operand(default = false)]
    pub strict: bool,
    #[operand(default = false)]
    pub env_clear: bool,
    pub ambient_allowlist: Vec<String>,
    pub idle_archive_secs: Option<i64>,
    #[operand(default = 0)]
    pub max_concurrent: i64,
    pub turn_budget: Option<i64>,
    #[operand(default = String::from("weaver"))]
    pub prelude: String,
    /// Organization-owned instructions appended to this profile's opening
    /// prompt for every launch origin.
    #[operand(default = String::new())]
    pub instructions: String,
    #[operand(default = false)]
    pub restricted: bool,
    /// Repositories for which Loom may broker a short-lived GitHub App
    /// token.
    pub github_repositories: Vec<String>,
    /// Provider-specific fallback permissions.
    pub runtime_permissions: Vec<String>,
    /// Provider-neutral MCP selection: `none`, `all`, or `groups`.
    #[operand(json, default = McpAccess::default())]
    pub mcp_access: McpAccess,
}

pub type Output = ProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
