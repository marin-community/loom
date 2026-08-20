//! The managed repository store, the clone allowlist, and per-repo
//! environment variables.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod branches {
    use super::prelude::*;

    /// List the local git branches of a repo checkout, and which has a worktree.
    ///
    /// `cwd` is a server-local filesystem path (any git checkout the server
    /// process can read), not a managed-repo slug.
    #[operation(
    id = "repos.branches",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos branches",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// A path inside the repo checkout to list branches for.
        #[operand(positional)]
        pub cwd: String,
    }

    pub type Output = Vec<RepoBranchView>;
}

pub mod env {
    //! Per-repo environment variables — write-only values layered into a
    //! non-restricted session's terminal above its selected profile.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one per-repo environment variable. Removing an absent name is a
        /// no-op. Returns the refreshed metadata list (no values).
        #[operation(
    id = "repos.env.delete",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos env delete",
)]
        pub struct Delete;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The variable's name.
            #[operand(positional)]
            pub name: String,
            /// Repo to scope to (canonical primary-worktree path). One of
            /// `repo_root`/`cwd` is required.
            pub repo_root: Option<String>,
            /// A directory inside the repo, resolved server-side when `repo_root` is
            /// omitted.
            pub cwd: Option<String>,
        }

        pub type Output = RepoEnvView;
    }

    pub mod get {
        use super::prelude::*;

        /// Read a repo's environment variables' metadata: names and timestamps only
        /// — values are write-only and never returned.
        #[operation(
    id = "repos.env.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos env get",
)]
        pub struct Get;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// Repo to scope to (canonical primary-worktree path). One of
            /// `repo_root`/`cwd` is required.
            pub repo_root: Option<String>,
            /// A directory inside the repo, resolved server-side when `repo_root` is
            /// omitted.
            pub cwd: Option<String>,
        }

        pub type Output = RepoEnvView;
    }

    pub mod set {
        use super::prelude::*;

        /// Upsert one per-repo environment variable. The name is validated as a shell
        /// identifier that isn't one of loom's reserved control or GitHub credential
        /// names, so it can't corrupt or shadow the launch environment. Returns the
        /// refreshed metadata list (no values).
        #[operation(
    id = "repos.env.set",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos env set",
)]
        pub struct Set;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// The variable's name.
            #[operand(positional)]
            pub name: String,
            /// The value to store.
            #[operand(positional)]
            pub value: String,
            /// Repo to scope to (canonical primary-worktree path). One of
            /// `repo_root`/`cwd` is required.
            pub repo_root: Option<String>,
            /// A directory inside the repo, resolved server-side when `repo_root` is
            /// omitted.
            pub cwd: Option<String>,
        }

        pub type Output = RepoEnvView;
    }
}

pub mod list {
    use super::prelude::*;

    /// List the registered managed repos (the clone allowlist).
    #[operation(
    id = "repos.list",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos list",
)]
    pub struct List;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = Vec<RepoView>;
}

pub mod recent {
    use super::prelude::*;

    /// Recently-used repositories, most recent first — the launch flow's repo
    /// picker.
    #[operation(
    id = "repos.recent",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos recent",
)]
    pub struct Recent;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// Maximum repos to return (1-50); defaults to 10.
        pub limit: Option<i64>,
    }

    pub type Output = Vec<RecentRepoView>;
}

pub mod register {
    use super::prelude::*;

    /// Register a repo in the managed store — add it to the clone allowlist. The
    /// clone itself is lazy (it happens on first use); this just adds an entry.
    #[operation(
    id = "repos.register",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos register",
)]
    pub struct Register;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {
        /// A GitHub `owner/name` slug or a clone URL.
        #[operand(positional)]
        pub repo: String,
    }

    pub type Output = RepoView;
}

pub mod revisions {
    //! Validating a launch fork point against a repo checkout.
    pub(super) use super::prelude;
    pub mod validate {
        use super::prelude::*;

        /// Check whether a worktree fork point resolves against a repo checkout,
        /// matching what a launch would fork from — fetching the revision from
        /// `origin` on demand if needed. Never touches local branches or the working
        /// tree.
        #[operation(
    id = "repos.revisions.validate",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
    cli = "repos revisions validate",
)]
        pub struct Validate;

        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
        pub struct Input {
            /// A path inside the repo checkout to validate against.
            #[operand(positional)]
            pub cwd: String,
            /// The revision (branch, tag, or ref) to resolve.
            #[operand(positional)]
            pub revision: String,
        }

        pub type Output = RepoRevisionValidationView;
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    <list::List as Operation>::SPEC,
    <register::Register as Operation>::SPEC,
    <recent::Recent as Operation>::SPEC,
    <branches::List as Operation>::SPEC,
    <revisions::validate::Validate as Operation>::SPEC,
    <env::get::Get as Operation>::SPEC,
    <env::set::Set as Operation>::SPEC,
    <env::delete::Delete as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "repos",
        label: "Managed repositories",
        operations: OPERATIONS,
    }
}
