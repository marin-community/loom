//! Session lifecycle, status projection, and normalized history.
//!
//! The bootstrap read operation (`self.get`) lives in this bundle, named
//! `context` because `self` is reserved as a module name.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod adopt {
    use super::prelude::*;

    /// Rejoin an orphaned session to the active fleet: recreate its terminal (or
    /// resume its ACP runtime) in place, without touching the worktree or branch.
    #[operation(id = "sessions.adopt", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions adopt")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionView;
}

pub mod archive {
    use super::prelude::*;

    /// Archive a session: tear down its terminal and worktree, keeping the branch,
    /// its commits, the session row, and run history. The inverse of `recover`.
    #[operation(id = "sessions.archive", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions archive")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionArchiveResult;
}

pub mod changes {
    use super::prelude::*;

    /// The session's uncommitted worktree changes against its base branch.
    #[operation(id = "sessions.changes", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions changes")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = ChangeSetDto;
}

pub mod chat {
    use super::prelude::*;

    // `sessions.chat.stream` is the live half of this operation.
    pub(super) use super::prelude;
    pub mod stream {
        use super::prelude::*;

        /// Subscribe to an ACP session's assistant token deltas.
        ///
        /// Available only for ACP sessions. Terminal sessions have no token stream.
        #[operation(id = "sessions.chat.stream", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], io = Stream)]
        pub struct Input {
            #[serde(default)]
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ();
    }

    /// The journaled ACP conversation plus the agent-owned composer metadata,
    /// paged newest-first.
    #[operation(id = "sessions.chat", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions chat")]
    pub struct Input {
        /// Page before this turn (paired with `before_seq`).
        pub before_turn: Option<i64>,
        /// Page before this sequence number within `before_turn`.
        pub before_seq: Option<i64>,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionChatView;
}

pub mod config {
    //! An ACP session's agent-owned configuration selectors.
    pub(super) use super::prelude;
    pub mod set {
        use super::prelude::*;

        /// Change one agent-owned session configuration selector. Waits for the
        /// adapter's response and returns its full refreshed option list (also
        /// broadcast to chat clients as a `metadata` event).
        #[operation(id = "sessions.config.set", actor = SessionSelf, scope = Session, risk = Write,
                    grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// Which configuration selector to change.
            #[operand(positional)]
            pub config_id: String,
            /// The new value for this option.
            #[operand(json)]
            pub value: serde_json::Value,
            #[operand(context)]
            pub session: String,
        }

        /// Result of `sessions.config.set`.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
        pub struct ConfigOptionResult {
            pub config_id: String,
            pub value: serde_json::Value,
            pub metadata: AcpMetadataView,
        }

        pub type Output = ConfigOptionResult;
    }
}

pub mod context {
    use super::prelude::*;

    /// Resolve this caller's session, branch, repository, channel, and links.
    // No CLI: `loom context` already names the credential-context manager.
    #[operation(id = "sessions.context", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"])]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SelfContextView;
}

pub mod conversation {
    use super::prelude::*;

    // `sessions.conversation.block` fetches untruncated content when an elided
    // block is referenced.
    pub(super) use super::prelude;
    pub mod block {
        use super::prelude::*;

        /// Fetch a full conversation block that was elided in the main conversation
        /// view, addressed by message and block position.
        #[operation(id = "sessions.conversation.block", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/sessions/read@v1"])]
        pub struct Input {
            /// Which message in the conversation.
            #[operand(positional)]
            pub message: u32,
            /// Which block within that message.
            pub block: u32,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = weaver_core::transcript::iris::Block;
    }

    /// The session's agent conversation as a normalized iris log — the live
    /// transcript when present, else the capture archived alongside it. Oversized
    /// tool payloads are elided to a preview naming `sessions.conversation.block`
    /// and the coordinates that fetch the rest.
    #[operation(id = "sessions.conversation", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions conversation")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = weaver_core::transcript::iris::Log;
}

pub mod delete {
    use super::prelude::*;

    /// Fully remove a session: tear down its terminal/worktree and, unless
    /// `keep_branch` is set, the branch and its commits too. The session row and
    /// run history are removed as well. This is irreversible; see `sessions.archive`
    /// to keep session data.
    #[operation(id = "sessions.delete", actor = SessionSelf, scope = Session, risk = Destructive,
                grants = ["loom/sessions/write@v1"])]
    pub struct Input {
        /// Keep the branch (and its commits) instead of deleting it along with
        /// the session.
        #[operand(default = false)]
        pub keep_branch: bool,
        #[operand(context)]
        pub session: String,
    }

    /// Result of `sessions.delete`. `kind` is `"session"` for a real session or
    /// `"launch_attempt"` when the id named a reservation that never became one,
    /// mirroring [`super::archive::Op`]'s result.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    pub struct DeleteResult {
        pub deleted: bool,
        pub kind: String,
        #[serde(default)]
        pub warnings: Vec<String>,
    }

    pub type Output = DeleteResult;
}

pub mod events {
    //! One session's lifecycle events, durable and live.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Record a trusted agent lifecycle event.
        #[operation(id = "sessions.events.create", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"], cli = "hook")]
        pub struct Input {
            /// The event kind, e.g. an agent hook name.
            #[operand(long = "event")]
            pub kind: String,
            /// Arbitrary event payload.
            #[operand(json, default = serde_json::Value::Null)]
            pub data: serde_json::Value,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = weaver_core::events::Event;
    }

    pub mod list {
        use super::prelude::*;

        /// List recent durable session events.
        #[operation(id = "sessions.events.list", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "sessions events")]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = Vec<weaver_core::events::Event>;
    }

    pub mod stream {
        use super::prelude::*;

        /// Subscribe to one session's live event feed.
        #[operation(id = "sessions.events.stream", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/sessions/read@v1"], io = Stream)]
        pub struct Input {
            // `serde(default)`: stream operands arrive via the query string,
            // extracted before the dispatcher's default-filling step runs.
            #[serde(default)]
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ();
    }
}

pub mod files {
    use super::prelude::*;

    /// Worktree file completion for the chat composer: tracked plus unignored
    /// untracked paths, optionally filtered by a case-insensitive substring.
    #[operation(id = "sessions.files", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions files")]
    pub struct Input {
        /// Case-insensitive substring filter. Blank matches everything.
        #[operand(default = String::new())]
        pub q: String,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionFilesView;
}

pub mod get {
    use super::prelude::*;

    /// Inspect one session and its branch projection.
    #[operation(id = "sessions.get", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions get")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionView;
}

pub mod github {
    //! A session's own pull-request association: which PR its branch is pinned
    //! to, and re-fetching/labeling it through Loom's GitHub App credential.
    //!
    //! Distinct from `permissions.github.{grant,revoke}`, which govern *repository
    //! access* for a session — a different resource entirely.
    pub(super) use super::prelude;
    pub mod access {
        //! Which repositories a session may reach through Loom's GitHub credential.
        //!
        //! Read-only. Granting and revoking are `permissions.github.grant` /
        //! `.revoke`, which live with the other permission decisions a human makes
        //! about an agent.
        pub(super) use super::prelude;
        pub mod list {
            use super::prelude::*;

            /// List the repository access a session has been granted.
            ///
            /// A human read *about* an agent, not something the agent can
            /// introspect about itself. A session that wants to know what it
            /// may reach asks GitHub, or fails and reads the error.
            #[operation(id = "sessions.github.access.list", actor = User, scope = Session,
                        risk = Read, cli = "sessions github access")]
            pub struct Input {
                /// A visible session id.
                #[operand(positional)]
                pub session: String,
            }

            pub type Output = Vec<SessionGithubAccessView>;
        }
    }

    pub mod clear {
        use super::prelude::*;

        /// Clear an explicit PR mapping and return to automatic current-open-PR
        /// discovery.
        #[operation(id = "sessions.github.clear", actor = SessionSelf, scope = Session,
                    risk = ExternalWrite, grants = ["loom/github/use@v1"])]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionView;
    }

    pub mod labels {
        pub(super) use super::prelude;
        pub mod add {
            use super::prelude::*;

            /// Add labels to the pull request currently associated with a session.
            ///
            /// Provides a Loom-owned interface for watch programs to add labels without
            /// needing direct GitHub credentials.
            #[operation(id = "sessions.github.labels.add", actor = SessionSelf, scope = Session,
                        risk = ExternalWrite, grants = ["loom/github/use@v1"])]
            pub struct Input {
                /// 1 to 10 label names to add to the pull request.
                #[serde(default)]
                pub labels: Vec<String>,
                #[operand(context)]
                pub session: String,
            }

            /// Result of `sessions.github.labels.add`.
            #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
            pub struct AddLabelsResult {
                pub number: i64,
                pub labels: Vec<String>,
            }

            pub type Output = AddLabelsResult;
        }
    }

    pub mod refresh {
        use super::prelude::*;

        /// Re-fetch the pull request currently associated with a session (by
        /// explicit mapping, or by automatic current-open-PR discovery) and refresh
        /// its cached status.
        #[operation(id = "sessions.github.refresh", actor = SessionSelf, scope = Session,
                    risk = ExternalWrite, grants = ["loom/github/use@v1"])]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionView;
    }

    pub mod set {
        use super::prelude::*;

        /// Pin a session's branch to an explicit pull request and fetch it
        /// immediately. The mapping is persisted only after GitHub confirms the
        /// number, so a typo never replaces a working association with a dead one.
        #[operation(id = "sessions.github.set", actor = SessionSelf, scope = Session,
                    risk = ExternalWrite, grants = ["loom/github/use@v1"])]
        pub struct Input {
            /// The pull request number to pin to.
            #[operand(positional)]
            pub pr_number: i64,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionView;
    }
}

pub mod handoff {
    use super::prelude::*;

    // `sessions.handoff.resolve` is the read-only preview half of this operation.
    pub(super) use super::prelude;
    pub mod resolve {
        use super::prelude::*;

        /// Preview a handoff without applying it: resolve a selection to the exact
        /// non-secret template that would be applied to the session.
        ///
        /// Same grant as `sessions.handoff` despite `risk = Read`: previewing
        /// needs no more authority than the write grant already covers, as with
        /// `sessions.launches.resolve`.
        #[operation(id = "sessions.handoff.resolve", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// The profile and per-launch overrides to resolve.
            #[operand(skip_cli)]
            pub selection: LaunchSelection,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ResolvedLaunchView;
    }

    /// Replace the provider behind an idle ACP session while preserving Loom's
    /// stable session/branch/worktree identity and canonical journal.
    #[operation(id = "sessions.handoff", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions handoff")]
    pub struct Input {
        /// Runtime selector (deprecated; use `selection` instead).
        #[operand(default = String::new())]
        pub agent: String,
        /// Blank/absent uses the target runtime's default.
        pub model: Option<String>,
        /// Blank/absent uses the target runtime's default.
        pub effort: Option<String>,
        /// ACP permission posture. Blank/absent uses the configured `agent.mode`.
        pub mode: Option<String>,
        /// The resolved profile and per-launch overrides, previewed beforehand.
        #[operand(skip_cli)]
        pub selection: Option<LaunchSelection>,
        /// Optimistic-concurrency guard against the previewed profile.
        #[operand(skip_cli)]
        pub expected_profile_revision: Option<i64>,
        /// Optimistic-concurrency guard against the previewed resolver snapshot.
        #[operand(skip_cli)]
        pub expected_resolver_revision: Option<String>,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionView;
}

pub mod history {
    //! Normalized, provider-neutral session history.
    pub(super) use super::prelude;

    /// The bounds the operands below advertise. `loom_store::history::page`
    /// enforces them; the schemars attributes have to spell the numbers as
    /// literals, so this assertion keeps the two from drifting.
    pub const MAX_LIMIT: usize = 200;
    pub const MAX_QUERY_BYTES: usize = 1024;
    const _: () = assert!(MAX_LIMIT == 200 && MAX_QUERY_BYTES == 1024);
    pub mod list {
        use super::prelude::*;

        /// Page this session's normalized records, newest tail first; follow
        /// `older_cursor` backward for the page before.
        #[operation(id = "sessions.history.list", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "sessions history",
                    render = custom)]
        pub struct Input {
            /// Page backward from this cursor (exclusive). Omit for the newest tail.
            #[schemars(length(min = 1))]
            pub before: Option<String>,
            /// Maximum records to return.
            #[schemars(range(min = 1, max = 200))]
            pub limit: Option<i64>,
            /// Restrict to these record kinds.
            #[serde(default)]
            #[schemars(extend("uniqueItems" = true))]
            pub kinds: Vec<HistoryKind>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = HistoryPageView;
    }

    pub mod search {
        use super::prelude::*;

        /// Search this session's normalized records for literal text, case
        /// insensitively, paging the same way `sessions.history.list` does.
        #[operation(id = "sessions.history.search", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/sessions/read@v1"])]
        pub struct Input {
            /// Case-insensitive literal search text.
            #[schemars(length(min = 1, max = 1024))]
            pub q: String,
            /// Page backward from this cursor (exclusive). Omit for the newest tail.
            #[schemars(length(min = 1))]
            pub before: Option<String>,
            /// Maximum records to return.
            #[schemars(range(min = 1, max = 200))]
            pub limit: Option<i64>,
            /// Restrict to these record kinds.
            #[serde(default)]
            #[schemars(extend("uniqueItems" = true))]
            pub kinds: Vec<HistoryKind>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = HistoryPageView;
    }
}

pub mod ide_info {
    use super::prelude::*;

    /// Whether the embedded editor (code-server) is enabled and runnable on this
    /// host, so a client can decide whether to offer it.
    #[operation(id = "sessions.ide_info", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions ide-info")]
    pub struct Input {}

    pub type Output = SessionIdeInfoView;
}

pub mod interrupt {
    use super::prelude::*;

    /// Interrupt a session's active turn.
    #[operation(id = "sessions.interrupt", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions interrupt")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionInterruptResult;
}

pub mod launch {
    use super::prelude::*;

    /// Launch a child session from a task or claimed work item.
    #[operation(id = "sessions.launch", actor = SessionSelf, scope = Global, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "launch")]
    pub struct Input {
        /// One-line task label for the new session.
        ///
        /// Optional: derived from a claimed issue or managed repo branch name if omitted.
        #[operand(positional)]
        pub title: Option<String>,
        /// Detailed goal for the new session; defaults to the task label.
        pub goal: Option<String>,
        /// A managed repository (GitHub `owner/name`) to launch against.
        pub repo: Option<String>,
        /// Local worktree path to fork the session's worktree from, when not
        /// launching against a managed `repo`.
        #[operand(default = String::new())]
        pub cwd: String,
        /// Base branch or ref to fork from.
        pub base: Option<String>,
        /// Agent runtime to launch; blank uses the profile's default.
        pub agent: Option<String>,
        /// Execution-backend override: `"terminal"` forces the PTY fallback for a
        /// builtin; `"acp"` opts in explicitly. Blank/absent uses the agent's
        /// declared default (acp for the builtins). Rejected for agents that don't
        /// support the requested backend.
        pub protocol: Option<String>,
        /// The ACP launch permission posture (`auto` | `bypassPermissions` |
        /// `acceptEdits` | `default` | `plan`). Blank/absent uses the configured
        /// `agent.mode` (which defaults to `auto`). Ignored for a terminal launch.
        pub mode: Option<String>,
        /// Session class override: `"interactive"` or `"automation"` (anything
        /// else is rejected). Blank/absent derives from the launch origin
        /// (watch/actions/ops/grafana → automation, else interactive).
        pub class: Option<String>,
        /// Named launch profile; blank selects `default`.
        pub profile: Option<String>,
        /// A pre-existing Loom backlog item to claim for this session.
        pub claim_issue: Option<i64>,
        /// An existing GitHub issue number to seed the session from.
        pub issue: Option<i64>,
        /// The branch of the launching session, when this is an agent-delegated
        /// launch. Filled from the caller's own branch; a human/dashboard launch
        /// leaves it unset.
        #[operand(context = "branch")]
        pub parent_branch: Option<String>,

        /// Explicit branch name instead of a generated one.
        pub name: Option<String>,
        /// Attach to a branch that already exists rather than creating one.
        pub existing_branch: Option<String>,
        /// A GitHub issue number to link the session to.
        pub github_issue: Option<i64>,
        /// Model override, when the profile's default is not wanted.
        pub model: Option<String>,
        /// Reasoning-effort override.
        pub effort: Option<String>,
        /// The resolved profile and per-launch overrides.
        ///
        /// Carries the agent, model, effort, and MCP access the caller previewed.
        #[operand(json, skip_cli)]
        pub selection: Option<LaunchSelection>,
        /// Files to seed the session's scratch directory with.
        #[serde(default)]
        #[operand(json, skip_cli)]
        pub scratch: Vec<ScratchUpload>,
        /// Optimistic-concurrency guards: the profile and resolver revisions the
        /// caller previewed against. A launch whose configuration changed underneath
        /// it is rejected rather than silently run with different settings.
        #[operand(skip_cli)]
        pub expected_profile_revision: Option<i64>,
        /// The resolver revision is a content hash, not a counter.
        #[operand(skip_cli)]
        pub expected_resolver_revision: Option<String>,
    }

    pub type Output = SessionView;
}

pub mod launches {
    //! Canonical session-launch template resolution (the read-only preview a
    //! launcher runs before `sessions.launch`).
    pub(super) use super::prelude;
    pub mod resolve {
        use super::prelude::*;

        /// Resolve a launch selection to its exact non-secret template snapshot —
        /// agent, model, effort, protocol, mode, capacity, and provenance — without
        /// launching a session. Not its own CLI verb; `loom sessions launch` runs
        /// this as its preflight.
        ///
        /// Read-only: a session authorized to delegate a child launch may preview
        /// the template it would launch with.
        #[operation(id = "sessions.launches.resolve", actor = SessionSelf, scope = Global,
                    risk = Read, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// The profile and per-launch overrides to resolve.
            #[operand(skip_cli)]
            pub selection: LaunchSelection,
        }

        pub type Output = ResolvedLaunchView;
    }
}

pub mod list {
    use super::prelude::*;

    /// List and search visible sessions.
    #[operation(id = "sessions.list", actor = SessionSelf, scope = Global, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions list")]
    pub struct Input {
        /// Case-insensitive search over title, goal, branch, and tags.
        #[operand(default = String::new())]
        pub q: String,
        /// Widen the search to include recently archived sessions.
        #[operand(default = false)]
        pub history: bool,
        /// Return only archived sessions (the History view).
        #[operand(default = false)]
        pub archived_only: bool,
        /// Filter by lifecycle status.
        #[operand(json, default = None)]
        pub status: Option<SessionSearchStatus>,
        /// Filter by attention level.
        #[operand(json, default = None)]
        pub attention: Option<SessionSearchAttention>,
        /// Filter by who created the session, relative to the caller.
        #[operand(json, default = None)]
        pub creator: Option<SessionCreatorFilter>,
        /// Include automation-class sessions.
        ///
        /// Defaults to true (a fleet listing means every session); `loom ps`
        /// passes `false` for an interactive-only inventory.
        #[operand(default = true)]
        pub automation: bool,
        /// Include engine-managed warm sessions.
        ///
        /// An operator inventory escape hatch, refused to anything but a human
        /// credential: normal fleet and survey callers must not see a watcher's own
        /// infrastructure, because a watch that can see its own warm session can
        /// recurse into it.
        #[operand(default = false)]
        pub managed: bool,
    }

    pub type Output = Vec<SessionView>;
}

pub mod mode {
    use super::prelude::*;

    /// Change an ACP session's permission mode (`session/set_mode`), journaling a
    /// `mode_change` block.
    #[operation(id = "sessions.mode", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions mode")]
    pub struct Input {
        /// The mode id to switch to, as advertised by the adapter's metadata.
        #[operand(positional)]
        pub mode_id: String,
        /// Who is changing it (a watch name, or blank for `manual`).
        pub by: Option<String>,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionModeResult;
}

pub mod permissions {
    //! Answering a live, in-flight ACP permission prompt.
    //!
    //! Distinct from `permissions.requests.{approve,deny}`, which resolve
    //! out-of-band request records.
    pub(super) use super::prelude;
    pub mod answer {
        use super::prelude::*;

        /// Answer a pending in-flight ACP permission prompt by its chosen option:
        /// 404 for an unknown request id, 409 when it was already resolved.
        #[operation(id = "sessions.permissions.answer", actor = User, scope = Session, risk = Write)]
        pub struct Input {
            /// The live permission request to answer.
            #[operand(positional)]
            pub request_id: String,
            /// The chosen option's id, as advertised by the prompt.
            pub option_id: String,
            /// Who is answering (a watch name, or blank for `manual`).
            pub by: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        /// Result of `sessions.permissions.answer`.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
        pub struct AnswerPermissionResult {
            pub resolved: bool,
            pub option_id: String,
        }

        pub type Output = AnswerPermissionResult;
    }
}

pub mod preview {
    use super::prelude::*;

    /// Read a bounded terminal preview.
    #[operation(id = "sessions.preview", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions preview")]
    pub struct Input {
        /// Extra scrollback lines to include above the visible screen (0 = just
        /// the visible pane).
        #[operand(default = 0)]
        pub lines: i64,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionPreviewResult;
}

pub mod prompt {
    //! Deliver or withdraw a queued user message to an ACP session's next turn.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Send a user message to an ACP session. Dispatched immediately when idle,
        /// or appended to the durable queue while a turn is live; `send_now` instead
        /// cancels an ordinary live turn and starts the message as a normal prompt.
        /// A live compaction always finishes before queued feedback starts. Every
        /// send records a `nudge` event on the branch.
        ///
        /// Provenance is derived from the credential: `manual` for a human operator,
        /// `agent` otherwise.
        #[operation(id = "sessions.prompt.create", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// The message text.
            #[operand(positional)]
            pub text: String,
            /// Cancel an ordinary live turn and start this message as a normal
            /// prompt. A live compaction queues it until its provider-owned
            /// boundary instead.
            #[operand(default = false)]
            pub send_now: bool,
            /// Promote the server's durable next-turn queue instead of sending
            /// `text`. Keeps the action race-free when a client is showing queued
            /// copy.
            #[operand(default = false)]
            pub force_queued: bool,
            /// Worktree-relative files to attach as ACP resource links.
            #[serde(default)]
            pub files: Vec<String>,
            #[operand(context)]
            pub session: String,
        }

        /// Result of `sessions.prompt.create`. Mirrors the ACP task's own
        /// acknowledgement (`queued`, `turn`), the same shape `sessions.send` returns
        /// for an ACP session.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
        pub struct PromptResult {
            pub queued: bool,
            pub turn: Option<i64>,
        }

        pub type Output = PromptResult;
    }

    pub mod retract {
        use super::prelude::*;

        /// Pull unseen next-turn feedback back out of the durable queue for editing.
        /// The ACP task owns the consume so this action is serialized with automatic
        /// dispatch at a turn boundary.
        #[operation(id = "sessions.prompt.retract", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        /// Result of `sessions.prompt.retract`: the retracted text.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
        pub struct RetractResult {
            pub text: String,
        }

        pub type Output = RetractResult;
    }
}

pub mod raw {
    use super::prelude::*;

    /// Raw bytes of a worktree file, with a guessed content type — for inline
    /// image previews and downloads. Always reads the working tree, never a git ref.
    ///
    /// `io = Download`: the browser fetches this directly and needs raw bytes
    /// rather than a JSON envelope.
    #[operation(id = "sessions.raw", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], io = Download)]
    pub struct Input {
        /// Worktree-relative path to read.
        //
        // `serde(default)`: download operands arrive via the query string,
        // extracted before defaults fill in. Empty path is rejected by the handler.
        #[serde(default)]
        #[operand(default = String::new())]
        pub path: String,
        #[serde(default)]
        #[operand(context)]
        pub session: String,
    }

    pub type Output = ();
}

pub mod recover {
    use super::prelude::*;

    /// Recover an archived session: rebuild its worktree from the kept branch, then
    /// resume the agent. For a live (non-archived) session, restart its ACP
    /// runtime instead. The inverse of `archive`.
    #[operation(id = "sessions.recover", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions recover")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionView;
}

pub mod resumption_cue {
    //! The short summary shown when returning to an idle session.
    pub(super) use super::prelude;
    pub mod ensure {
        use super::prelude::*;

        /// Generate the session's resumption cue if it is missing or stale. `force`
        /// regenerates it unconditionally; otherwise the configured inactivity
        /// threshold applies, as on the on-return path.
        #[operation(id = "sessions.resumption_cue.ensure", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// Regenerate unconditionally instead of respecting the inactivity
            /// threshold.
            #[operand(default = false)]
            pub force: bool,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ResumptionCueView;
    }

    pub mod get {
        use super::prelude::*;

        /// The session's current resumption cue, if one has been generated.
        #[operation(id = "sessions.resumption_cue.get", actor = SessionSelf, scope = Session,
                    risk = Read, grants = ["loom/sessions/read@v1"])]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ResumptionCueView;
    }
}

pub mod scratch {
    //! A session's Scratch directory: the files handed to it at launch and the
    //! ones written to it while it runs.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Delete one Scratch file.
        #[operation(id = "sessions.scratch.delete", actor = SessionSelf, scope = Session,
                    risk = Destructive, grants = ["loom/sessions/write@v1"],
                    cli = "sessions scratch delete")]
        pub struct Input {
            /// The file name to delete.
            #[operand(positional)]
            pub name: String,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ScratchDeleteResult;
    }

    pub mod limits {
        use super::prelude::*;

        /// Shared upload limits for launch-time and live-session Scratch attachments.
        #[operation(id = "sessions.scratch.limits", actor = User, scope = Global, risk = Read,
                    cli = "sessions scratch limits")]
        pub struct Input {}

        pub type Output = ScratchLimitsView;
    }

    pub mod list {
        use super::prelude::*;

        /// List a session's Scratch files.
        #[operation(id = "sessions.scratch.list", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "sessions scratch list")]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = Vec<ScratchFileView>;
    }

    pub mod write {
        use super::prelude::*;

        /// Write one Scratch file from a raw request body.
        ///
        /// The only `io = Upload` operation: the body is the file's bytes, so
        /// operands arrive in the query string instead of a JSON envelope.
        /// Launch-time attachments instead go base64-encoded inside
        /// `sessions.launch`'s JSON, since that request carries several files
        /// plus the rest of the launch configuration.
        #[operation(id = "sessions.scratch.write", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"], io = Upload)]
        pub struct Input {
            /// The file name to write, a single path component.
            //
            // `serde(default)`: upload operands arrive via the query string,
            // extracted before defaults fill in. Empty name is rejected by the handler.
            #[serde(default)]
            #[operand(default = String::new())]
            pub name: String,
            #[serde(default)]
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ScratchWriteResult;
    }
}

pub mod send {
    use super::prelude::*;

    /// Deliver a new prompt to a session.
    #[operation(id = "sessions.send", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], cli = "sessions send")]
    pub struct Input {
        /// The text to type into the agent's pane.
        #[operand(positional)]
        pub text: String,
        /// Whether to follow the text with Enter to submit it as a turn. Omit for
        /// the default (submit); pass `false` to stage input unsubmitted.
        pub submit: Option<bool>,
        /// Who is sending (a watch name, or blank for `manual`).
        pub by: Option<String>,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionSendResult;
}

pub mod shells {
    //! Live worktree debug shells for a session: which are open, closing one, and
    //! attaching to one.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Close one of a session's worktree debug shells, killing its supervisor.
        #[operation(id = "sessions.shells.delete", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            /// Which of the session's debug shells to close.
            #[operand(positional)]
            pub index: u32,
            #[operand(context)]
            pub session: String,
        }

        /// The shell indices still live after the close, so a client refreshes its tabs
        /// in one round trip.
        pub type Output = Vec<u32>;
    }

    pub mod list {
        use super::prelude::*;

        /// The live worktree debug-shell indices for a session, so a client re-opens
        /// the shell tabs after a reload. Never spawns.
        #[operation(id = "sessions.shells.list", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "sessions shells")]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = Vec<u32>;
    }

    pub mod terminal {
        use super::prelude::*;

        /// Attach to one of a session's worktree debug shells over a websocket.
        ///
        /// The target is a plain login shell in the session's worktree, spawned on first
        /// attach. This is `risk = ExternalWrite` because it runs arbitrary commands as
        /// the operator inside the session's checkout.
        #[operation(id = "sessions.shells.terminal", actor = SessionSelf, scope = Session,
                    risk = ExternalWrite, grants = ["loom/sessions/write@v1"], io = Duplex)]
        pub struct Input {
            /// Which of the session's debug shells; several may run at once.
            #[serde(default)]
            #[operand(default = 0u32)]
            pub index: u32,
            #[serde(default)]
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ();
    }
}

pub mod status {
    //! The session's durable attention level and status message.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Read the session's durable attention level and status message.
        #[operation(id = "sessions.status.get", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "status get", render = custom)]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = BranchView;
    }

    pub mod set {
        use super::prelude::*;

        /// Update the durable attention level and status message.
        #[operation(id = "sessions.status.set", actor = SessionSelf, scope = Session, risk = Write,
                    grants = ["loom/sessions/write@v1"], cli = "status set", render = custom)]
        pub struct Input {
            /// The attention level.
            #[operand(long = "tag")]
            #[schemars(extend("enum" = ["ok", "attention", "blocked"]))]
            pub level: String,
            /// The current-state message shown alongside the level.
            #[schemars(length(max = 4096))]
            pub message: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = BranchView;
    }
}

pub mod summary {
    //! The structured per-session catch-up, and the fleet index it reduces to.
    pub(super) use super::prelude;
    pub mod get {
        use super::prelude::*;

        /// Return the current goal, status, inbox, artifacts, issues, and next actions.
        #[operation(id = "sessions.summary.get", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "summary")]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionCatchupView;
    }

    pub mod list {
        use super::prelude::*;

        /// The fleet index: one compact row per visible session.
        ///
        /// A reduced projection, so a caller that only needs the compact view
        /// does not pay for the full one. `sessions.get` returns the whole
        /// context.
        #[operation(id = "sessions.summary.list", actor = User, scope = Global, risk = Read,
                    cli = "sessions summaries")]
        pub struct Input {
            /// Include archived rows alongside active work.
            #[operand(default = false)]
            pub archived: bool,
            /// Return only archived rows. Implies `archived`.
            #[operand(default = false)]
            pub archived_only: bool,
            /// Include automation-class sessions.
            #[operand(default = false)]
            pub automation: bool,
            /// Case-insensitive search over the same facets as fleet search.
            #[operand(default = String::new())]
            pub q: String,
            /// Filter by lifecycle status.
            #[operand(json, default = None)]
            pub status: Option<SessionSearchStatus>,
            /// Filter by attention level.
            #[operand(json, default = None)]
            pub attention: Option<SessionSearchAttention>,
            /// Filter by who created the session, relative to the caller.
            #[operand(json, default = None)]
            pub creator: Option<SessionCreatorFilter>,
        }

        pub type Output = Vec<SessionSummaryView>;
    }
}

pub mod tags {
    //! Free-form `(key, value)` annotations on a session's branch.
    pub(super) use super::prelude;
    pub mod delete {
        use super::prelude::*;

        /// Remove one free-form session tag.
        #[operation(id = "sessions.tags.delete", actor = SessionSelf, scope = Session, risk = Write,
                    grants = ["loom/sessions/write@v1"], cli = "sessions tags delete")]
        pub struct Input {
            /// The tag key to remove.
            #[operand(positional)]
            pub key: String,
            /// Who is clearing it (a watch name, or blank for `manual`).
            pub by: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = BranchView;
    }

    pub mod list {
        use super::prelude::*;

        /// List free-form tags on a session.
        #[operation(id = "sessions.tags.list", actor = SessionSelf, scope = Session, risk = Read,
                    grants = ["loom/sessions/read@v1"], cli = "sessions tags list")]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = BranchView;
    }

    pub mod replace {
        use super::prelude::*;

        /// Atomically replace one author's complete tag set on a session.
        ///
        /// Every row authored by `by` is replaced in one transaction, so a
        /// stale update cannot delete a key another actor took over since the
        /// caller last looked.
        #[operation(id = "sessions.tags.replace", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"],
                    cli = "sessions tags replace")]
        pub struct Input {
            /// The complete tag set this author now asserts.
            #[operand(json, default = Vec::new())]
            pub tags: Vec<TagInput>,
            /// Exact `(key, value)` pairs to clear in the same transaction.
            #[operand(json, default = Vec::new())]
            pub clear: Vec<TagMatch>,
            /// The author whose existing tag set is replaced. Defaults to `manual`.
            pub by: Option<String>,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionView;
    }

    pub mod set {
        use super::prelude::*;

        /// Set one free-form session tag.
        #[operation(id = "sessions.tags.set", actor = SessionSelf, scope = Session, risk = Write,
                    grants = ["loom/sessions/write@v1"], cli = "sessions tags set")]
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
            #[operand(context)]
            pub session: String,
        }

        pub type Output = BranchView;
    }
}

pub mod terminal {
    use super::prelude::*;

    /// Attach to a session's agent terminal over a websocket.
    ///
    /// `io = Duplex`: the response is a protocol upgrade, served by a custom
    /// handler.
    #[operation(id = "sessions.terminal", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"], io = Duplex)]
    pub struct Input {
        #[serde(default)]
        #[operand(context)]
        pub session: String,
    }

    pub type Output = ();
}

pub mod title {
    //! A session's generated title: the toggle for automatic generation, and an
    //! explicit one-shot regenerate.
    pub(super) use super::prelude;
    pub mod generation {
        pub(super) use super::prelude;
        pub mod set {
            use super::prelude::*;

            /// Toggle whether Loom generates this session's title automatically.
            #[operation(id = "sessions.title.generation.set", actor = SessionSelf, scope = Session,
                        risk = Write, grants = ["loom/sessions/write@v1"])]
            pub struct Input {
                /// Whether automatic title generation is enabled.
                #[operand(positional)]
                pub enabled: bool,
                #[operand(context)]
                pub session: String,
            }

            pub type Output = SessionView;
        }
    }

    pub mod regenerate {
        use super::prelude::*;

        /// Regenerate a session's title immediately, bypassing the confidence guard
        /// that normally throttles automatic generation.
        #[operation(id = "sessions.title.regenerate", actor = SessionSelf, scope = Session,
                    risk = Write, grants = ["loom/sessions/write@v1"])]
        pub struct Input {
            #[operand(context)]
            pub session: String,
        }

        pub type Output = SessionView;
    }
}

pub mod update {
    use super::prelude::*;

    /// Update a session's branch-level fields (title, goal, description) and its
    /// durable status. Attention level is managed via tags operations
    /// (`sessions.tags.set`/`sessions.tags.delete`).
    #[operation(id = "sessions.update", actor = SessionSelf, scope = Session, risk = Write,
                grants = ["loom/sessions/write@v1"])]
    pub struct Input {
        /// New durable status (the fleet lifecycle marker).
        pub status: Option<String>,
        /// New task label for the branch.
        pub title: Option<String>,
        /// Required with `title`: the label the caller last observed. Used to detect
        /// and reject concurrent updates by comparing with the current value.
        pub expected_title: Option<String>,
        /// Required with `title`: the provenance (`user` or `agent`) the caller
        /// last observed.
        pub expected_title_provenance: Option<String>,
        /// New goal text for the branch.
        pub goal: Option<String>,
        /// The agent's current-state message — the prose shown beside the
        /// attention level.
        pub description: Option<String>,
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionView;
}

pub mod url {
    use super::prelude::*;

    /// The externally-visible dashboard URL for a session. The agent inside a
    /// session only knows its loopback API address, so only the server can resolve
    /// this — from the configured `auth.base_url`, or the address it is bound to.
    #[operation(id = "sessions.url", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/sessions/read@v1"], cli = "sessions url")]
    pub struct Input {
        #[operand(context)]
        pub session: String,
    }

    pub type Output = SessionUrlView;
}

static OPERATIONS: &[&OperationSpec] = &[
    context::SPEC,
    summary::get::SPEC,
    summary::list::SPEC,
    list::SPEC,
    get::SPEC,
    launch::SPEC,
    launches::resolve::SPEC,
    send::SPEC,
    interrupt::SPEC,
    preview::SPEC,
    events::list::SPEC,
    events::create::SPEC,
    history::list::SPEC,
    history::search::SPEC,
    status::get::SPEC,
    status::set::SPEC,
    tags::list::SPEC,
    tags::set::SPEC,
    tags::replace::SPEC,
    tags::delete::SPEC,
    adopt::SPEC,
    archive::SPEC,
    recover::SPEC,
    handoff::SPEC,
    handoff::resolve::SPEC,
    changes::SPEC,
    chat::SPEC,
    conversation::SPEC,
    conversation::block::SPEC,
    files::SPEC,
    mode::SPEC,
    raw::SPEC,
    url::SPEC,
    ide_info::SPEC,
    shells::list::SPEC,
    shells::delete::SPEC,
    scratch::limits::SPEC,
    scratch::list::SPEC,
    scratch::write::SPEC,
    scratch::delete::SPEC,
    update::SPEC,
    delete::SPEC,
    config::set::SPEC,
    github::refresh::SPEC,
    github::set::SPEC,
    github::clear::SPEC,
    github::access::list::SPEC,
    github::labels::add::SPEC,
    prompt::create::SPEC,
    prompt::retract::SPEC,
    resumption_cue::get::SPEC,
    resumption_cue::ensure::SPEC,
    permissions::answer::SPEC,
    title::regenerate::SPEC,
    title::generation::set::SPEC,
    // Non-JSON: SSE feeds and terminal websockets. Registered exactly like the
    // rest — only the response encoding differs, so a custom handler in
    // `loom::web::encodings` serves them off these same declarations.
    events::stream::SPEC,
    chat::stream::SPEC,
    terminal::SPEC,
    shells::terminal::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "sessions",
        label: "Session workflow",
        operations: OPERATIONS,
    }
}
