//! Provider-neutral inspection and administration of Loom's MCP registry:
//! built-in domains, versioned capability sets, operator-authored custom
//! servers, and registered remote servers.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod remote {
    //! Operator-registered remote Streamable HTTP MCP servers.

    pub(super) use super::prelude;

    pub mod create {
        use super::prelude::*;

        /// Register a remote Streamable HTTP MCP server.
        #[operation(id = "mcps.remote.create", actor = Admin, scope = Global, risk = Write,
                    cli = "mcps remote create")]
        pub struct Input {
            #[operand(positional)]
            pub identity: String,
            pub label: String,
            #[operand(default = String::new())]
            pub description: String,
            pub url: String,
            #[operand(json, default = RemoteMcpAuth::default())]
            pub auth: RemoteMcpAuth,
            #[operand(default = true)]
            pub enabled: bool,
        }

        pub type Output = RemoteMcpView;
    }

    pub mod delete {
        use super::prelude::*;

        /// Remove a remote MCP server that no profile pins.
        #[operation(id = "mcps.remote.delete", actor = Admin, scope = Global, risk = Destructive,
                    cli = "mcps remote delete", cli_alias = "rm")]
        pub struct Input {
            #[operand(positional)]
            pub identity: String,
        }

        pub type Output = RemoteMcpDeleteResult;
    }

    pub mod get {
        use super::prelude::*;

        /// Show one remote MCP server.
        #[operation(id = "mcps.remote.get", actor = User, scope = Global, risk = Read,
                    cli = "mcps remote get")]
        pub struct Input {
            #[operand(positional)]
            pub identity: String,
        }

        pub type Output = RemoteMcpView;
    }

    pub mod list {
        use super::prelude::*;

        /// List registered remote MCP servers.
        #[operation(id = "mcps.remote.list", actor = User, scope = Global, risk = Read,
                    cli = "mcps remote list", cli_alias = "ls")]
        pub struct Input {}

        pub type Output = Vec<RemoteMcpView>;
    }

    pub mod update {
        use super::prelude::*;

        /// Replace a remote MCP definition and create a new pinned revision.
        #[operation(id = "mcps.remote.update", actor = Admin, scope = Global, risk = Write,
                    cli = "mcps remote update")]
        pub struct Input {
            #[operand(positional)]
            pub identity: String,
            pub label: String,
            #[operand(default = String::new())]
            pub description: String,
            pub url: String,
            #[operand(json, default = RemoteMcpAuth::default())]
            pub auth: RemoteMcpAuth,
            #[operand(default = true)]
            pub enabled: bool,
        }

        pub type Output = RemoteMcpView;
    }
}

pub mod custom {
    //! Operator-authored custom MCP servers: uv Python scripts Loom validates,
    //! versions, and can launch alongside the built-in aggregate server.

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

    /// The trusted MCP registry: built-in domains, versioned capability sets,
    /// and operator-authored custom servers.
    #[operation(id = "mcps.get", actor = User, scope = Global, risk = Read, cli = "mcps get",
                cli_alias = "ls", render = custom)]
    pub struct Input {}

    pub type Output = McpRegistryView;
}

static OPERATIONS: &[&OperationSpec] = &[
    get::SPEC,
    remote::list::SPEC,
    remote::get::SPEC,
    remote::create::SPEC,
    remote::update::SPEC,
    remote::delete::SPEC,
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
