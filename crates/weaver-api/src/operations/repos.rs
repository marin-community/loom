//! The managed repository store, the clone allowlist, and per-repo
//! environment variables.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod branches {
    use super::prelude::*;

    /// List the local git branches of a repo checkout, and which has a worktree.
    ///
    /// `cwd` is a server-local filesystem path (any git checkout the server
    /// process can read), not a managed-repo slug.
    #[operation(id = "repos.branches", actor = User, scope = Global, risk = Read,
                cli = "repos branches")]
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
        #[operation(id = "repos.env.delete", actor = User, scope = Global, risk = Write,
                    cli = "repos env delete")]
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
        #[operation(id = "repos.env.get", actor = User, scope = Global, risk = Read,
                    cli = "repos env get")]
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
        #[operation(id = "repos.env.set", actor = User, scope = Global, risk = Write,
                    cli = "repos env set")]
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
    #[operation(id = "repos.list", actor = User, scope = Global, risk = Read, cli = "repos list")]
    pub struct Input {}

    pub type Output = Vec<RepoView>;
}

pub mod recent {
    use super::prelude::*;

    /// Recently-used repositories, most recent first — the launch flow's repo
    /// picker.
    #[operation(id = "repos.recent", actor = User, scope = Global, risk = Read,
                cli = "repos recent")]
    pub struct Input {
        /// Maximum repos to return (1-50); defaults to 10.
        pub limit: Option<i64>,
    }

    pub type Output = Vec<RecentRepoView>;
}

pub mod register {
    use super::prelude::*;

    /// Register a repo in the managed store — add it to the clone allowlist. The
    /// clone itself is lazy (it happens on first use); this adds an entry.
    #[operation(id = "repos.register", actor = User, scope = Global, risk = Write,
                cli = "repos register")]
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
        #[operation(id = "repos.revisions.validate", actor = User, scope = Global, risk = Read,
                    cli = "repos revisions validate")]
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

pub mod worktrees {
    //! Ensuring an editable checkout exists for a branch.
    pub(super) use super::prelude;
    pub mod ensure {
        use super::prelude::*;

        /// Ensure a branch has a worktree checked out, creating one when it does
        /// not — the checkout-recovery half of “open this code” for a branch whose
        /// session was archived (archive keeps the branch but removes its
        /// worktree). Session-free on purpose: giving a human an editable checkout
        /// must not resurrect the session's agent or readmit it to the fleet.
        ///
        /// When `pr` is given, the branch is the PR's head branch (a fork PR is
        /// refused — its head lives in a repo loom cannot push to); `branch` names
        /// the branch directly. Idempotent: an existing worktree is returned, not
        /// recreated. The branch is materialized from `origin/<branch>` (fetching
        /// on demand) when it does not exist locally, so a never-checked-out PR
        /// head works too.
        #[operation(id = "repos.worktrees.ensure", actor = User, scope = Global, risk = Write,
                    cli = "repos worktrees ensure")]
        pub struct Input {
            /// A path inside the repo checkout to ensure the worktree under.
            #[operand(positional)]
            pub cwd: String,
            /// The branch to check out (e.g. `weaver/my-task`, or a PR's head
            /// branch).
            #[operand(long = "branch")]
            pub branch: Option<String>,
            /// A pull-request number whose head branch to check out. Takes
            /// precedence over `branch`; a cross-repo (fork) PR is refused.
            #[operand(long = "pr")]
            pub pr: Option<i64>,
        }

        pub type Output = RepoWorktreeView;
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    register::SPEC,
    recent::SPEC,
    branches::SPEC,
    revisions::validate::SPEC,
    worktrees::ensure::SPEC,
    env::get::SPEC,
    env::set::SPEC,
    env::delete::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "repos",
        label: "Managed repositories",
        operations: OPERATIONS,
    }
}
