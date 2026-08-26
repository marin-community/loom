//! Provider-neutral inspection and administration of Loom's MCP registry:
//! built-in adapters, versioned capability sets, and operator-authored
//! custom servers.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod custom {
    //! Operator-authored custom MCP servers: uv Python scripts Loom validates,
    //! versions, and can launch alongside the built-in adapters.

    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Add an operator-authored custom MCP server.
        #[operation(id = "mcps.custom.create", actor = Admin, scope = Global, risk = Write,
                    cli = "mcps custom create")]
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
    }

    pub mod delete {
        use super::prelude::*;

        /// Permanently remove an operator-authored custom MCP server.
        #[operation(id = "mcps.custom.delete", actor = Admin, scope = Global, risk = Destructive,
                    cli = "mcps custom delete", cli_alias = "rm")]
        pub struct Input {
            /// Absolute identity, e.g. `/engineering/search/docs`.
            #[operand(positional)]
            pub identity: String,
        }

        pub type Output = CustomMcpDeleteResult;
    }

    pub mod get {
        use super::prelude::*;

        /// Show one operator-authored custom MCP server's latest definition and
        /// validation state.
        #[operation(id = "mcps.custom.get", actor = User, scope = Global, risk = Read,
                    cli = "mcps custom get")]
        pub struct Input {
            /// Absolute identity, e.g. `/engineering/search/docs`.
            #[operand(positional)]
            pub identity: String,
        }

        pub type Output = CustomMcpView;
    }

    pub mod list {
        use super::prelude::*;

        /// List operator-authored custom MCP servers.
        #[operation(id = "mcps.custom.list", actor = User, scope = Global, risk = Read,
                    cli = "mcps custom list", cli_alias = "ls")]
        pub struct Input {}

        pub type Output = Vec<CustomMcpView>;
    }

    pub mod update {
        use super::prelude::*;

        /// Replace an operator-authored custom MCP server's definition, producing a
        /// new validated revision.
        #[operation(id = "mcps.custom.update", actor = Admin, scope = Global, risk = Write,
                    cli = "mcps custom update")]
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
    }
}

pub mod get {
    use super::prelude::*;

    /// The trusted MCP registry: built-in adapters, versioned capability sets,
    /// and operator-authored custom servers.
    #[operation(id = "mcps.get", actor = User, scope = Global, risk = Read, cli = "mcps get",
                render = custom)]
    pub struct Input {}

    pub type Output = McpRegistryView;
}

static OPERATIONS: &[&OperationSpec] = &[
    get::SPEC,
    custom::list::SPEC,
    custom::get::SPEC,
    custom::create::SPEC,
    custom::update::SPEC,
    custom::delete::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "mcps",
        label: "MCP registry",
        operations: OPERATIONS,
    }
}
