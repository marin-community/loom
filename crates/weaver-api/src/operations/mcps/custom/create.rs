use super::prelude::*;

/// Add an operator-authored custom MCP server.
#[operation(
    id = "mcps.custom.create",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "mcps custom create",
)]
pub struct Create;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Absolute identity, e.g. `/engineering/search/docs`.
    #[operand(positional)]
    pub identity: String,
    /// Display label.
    pub label: String,
    #[operand(default = String::new())]
    pub description: String,
    /// A uv Python script with PEP 723 inline dependencies. On the command
    /// line this names a file, or `-`/omitted to read stdin.
    #[operand(positional, from_file)]
    pub source: String,
    /// Optional uv Python test script.
    #[operand(default = String::new())]
    pub test_source: String,
    #[operand(default = true)]
    pub enabled: bool,
}

pub type Output = CustomMcpView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
