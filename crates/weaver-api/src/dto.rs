//! The cross-process wire contract: the request and response (View) DTOs the
//! loom REST API speaks. These are the single source of truth — the loom server
//! serializes them, the typed [`crate::Client`] and the future Python binding
//! deserialize them, and `frontend/types.ts` mirrors them by hand.
//!
//! The response (`*View`) types carry `from_parts` constructors that build a
//! plain wire struct from the `weaver-core` domain types (`Branch`, `Issue`,
//! `Watch`, …). The async server-side builders that touch the database
//! (counting open issues, joining the latest run) stay in the loom server and
//! call these once they've gathered the parts — so the wire struct has exactly
//! one definition while the DB access stays where the daemon owns it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use weaver_core::branch::Branch;
use weaver_core::github::GithubStatus;
use weaver_core::issue::Issue;
use weaver_core::tags::Tag;
use weaver_core::watch::{Watch, WatchRun};

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
        pub enum $name {
            $(
                #[serde(rename = $value)]
                $variant,
            )+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(format!("invalid {} '{value}'", stringify!($name))),
                }
            }
        }
    };
}

// ---------------------------------------------------------------------------
// View payloads (responses)
// ---------------------------------------------------------------------------

/// One tag on a branch, as the API exposes it. A `(key, value)` annotation with
/// a reason, author, and timestamp. The well-known keys are `attention` (the
/// agent's self-report) and `triage` (a watch's assessment); any other key
/// is a free-form, quiet pill. Absence is the calm state — there is no `ok` tag.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagView {
    pub key: String,
    pub value: String,
    pub note: String,
    pub set_by: String,
    pub set_at: String,
}

impl From<&Tag> for TagView {
    fn from(t: &Tag) -> Self {
        TagView {
            key: t.key.clone(),
            value: t.value.clone(),
            note: t.note.clone(),
            set_by: t.set_by.clone(),
            set_at: t.set_at.clone(),
        }
    }
}

/// Branch with denormalized open-issue count, returned by `/api/branches` and
/// embedded under `SessionView::branch`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BranchView {
    pub id: String,
    /// Short label: the branch name with the optional `weaver/` prefix stripped.
    pub name: String,
    pub title: String,
    /// Ownership of the unqualified task label: `derived`, `generated`,
    /// `user`, or `issue`.
    #[serde(default = "default_title_provenance")]
    pub title_provenance: String,
    pub goal: String,
    /// The agent's current-state message, set via `loom status`, shown even
    /// when the branch is calm. The attention *level* is the `attention` tag.
    pub description: String,
    /// Every tag on the branch (the agent's `attention`, a watch's
    /// `triage`, and any free-form key), ordered by key. Empty when the branch is
    /// calm and unmarked — absence is the default state, there is no `ok` tag.
    pub tags: Vec<TagView>,
    pub repo_root: String,
    pub branch: String,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub open_issue_count: i64,
    /// The branch's latest GitHub pull-request snapshot (link, review decision,
    /// check rollup), or `null` when GitHub polling is off, the repo has no
    /// remote PR, or `gh` is unavailable. Maintained by the loom poll loop.
    pub github: Option<GithubStatus>,
    /// A user-selected PR number. `null` means loom discovers the branch's
    /// current open PR automatically; this is deliberately separate from the
    /// cached `github` snapshot above.
    pub github_pr: Option<i64>,
}

impl BranchView {
    /// Build the wire view from a branch plus the parts the server gathered (its
    /// tags, the open-issue count, and the latest GitHub snapshot). The async DB
    /// lookups that produce those parts live in the loom server.
    pub fn from_parts(
        branch: &Branch,
        tags: &[Tag],
        open_issue_count: i64,
        github: Option<GithubStatus>,
        github_pr: Option<i64>,
    ) -> Self {
        let name = branch
            .branch
            .strip_prefix("weaver/")
            .unwrap_or(&branch.branch)
            .to_string();
        BranchView {
            id: branch.id.clone(),
            name,
            title: branch.title.clone(),
            title_provenance: branch.title_provenance.as_str().to_string(),
            goal: branch.goal.clone(),
            description: branch.description.clone(),
            tags: tags.iter().map(TagView::from).collect(),
            repo_root: branch.repo_root.clone(),
            branch: branch.branch.clone(),
            base_branch: branch.base_branch.clone(),
            created_at: branch.created_at.clone(),
            updated_at: branch.updated_at.clone(),
            open_issue_count,
            github,
            github_pr,
        }
    }
}

/// Compact branch projection embedded in [`SessionSummaryView`]. It carries
/// only the identity, status, search, and GitHub fields fleet surfaces render;
/// large goal text and detail-only metadata remain on [`BranchView`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BranchSummaryView {
    pub id: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<TagView>,
    pub repo_root: String,
    pub branch: String,
    pub github: Option<GithubStatus>,
    pub github_pr: Option<i64>,
}

impl From<&BranchView> for BranchSummaryView {
    fn from(branch: &BranchView) -> Self {
        Self {
            id: branch.id.clone(),
            name: branch.name.clone(),
            title: branch.title.clone(),
            description: branch.description.clone(),
            tags: branch.tags.clone(),
            repo_root: branch.repo_root.clone(),
            branch: branch.branch.clone(),
            github: branch.github.clone(),
            github_pr: branch.github_pr,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionTransitionView {
    /// Stable operation name: currently `archiving` or `adopting`.
    pub kind: String,
    /// Human-readable current stage, suitable for direct UI presentation.
    pub step: String,
    /// ISO timestamp at which this operation claimed the session.
    pub started_at: String,
}

/// Compact session projection returned by `GET /api/sessions/summary`.
///
/// This is the polling/search contract for fleet indexes. A client follows with
/// `GET /api/sessions/{id}` only when it opens a session or discloses the row's
/// complete context.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSummaryView {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub transition: Option<SessionTransitionView>,
    pub github_repo: Option<String>,
    #[serde(default)]
    pub github_issue: Option<GithubIssueRef>,
    pub last_activity_at: String,
    pub created_at: String,
    pub parent_id: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub created_by: Option<String>,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default = "default_class")]
    pub class: String,
    pub tracking_issue: Option<i64>,
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub usage: Option<AcpUsage>,
    #[serde(default)]
    pub placement: Option<SessionPlacementView>,
    pub branch: BranchSummaryView,
}

/// Session-scoped view returned by the `/api/sessions[/...]` endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionView {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub transition: Option<SessionTransitionView>,
    pub work_dir: String,
    pub term_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub github_repo: Option<String>,
    /// GitHub issue linked to this session's explicit work item, if any. This is
    /// separate from `branch.github`, which is the pull request created by the
    /// work. The compatibility work item remains the source of truth for edits.
    #[serde(default)]
    pub github_issue: Option<GithubIssueRef>,
    pub last_activity_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Optional metadata-agent state for the task label.
    #[serde(default)]
    pub title_generation: TitleGenerationView,
    /// Branch id of the session that **launched** this one — the parent in the
    /// dashboard's session tree — or `null` for a top-level session.
    pub parent_id: Option<String>,
    /// Exact immutable session id of the launcher. New rows always stamp this.
    /// `parent_id` is retained for backward compatibility with older sessions.
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// The principal (username) that launched this session — attribution for the
    /// shared team board. `null` for engine-created warm watch sessions and rows
    /// that predate the column. A tracking/UX field,
    /// not a security boundary: the fleet stays co-owned by everyone authenticated.
    pub created_by: Option<String>,
    /// How this session came to exist: `"user"` (hand-launched), `"agent"`
    /// (delegated by another session), `"github"` / `"slack"` (chat triggers),
    /// `"watch"` (engine infrastructure). Stamped once at create.
    #[serde(default = "default_origin")]
    pub origin: String,
    /// Machine tier: `"interactive"` or `"automation"`. Both appear in the
    /// normal fleet; the class remains useful for policy and compatibility
    /// filters.
    #[serde(default = "default_class")]
    pub class: String,
    /// Completed agent turns on this session.
    #[serde(default)]
    pub turn_count: i64,
    /// An explicit claimed/imported compatibility work item. Ordinary sessions
    /// coordinate through their same-id channel and leave this `null`.
    pub tracking_issue: Option<i64>,
    /// Legacy compatibility read derived from canonical placement. `"parked"`
    /// means the session currently belongs to a system `Later` group; all
    /// other placements read as `null`.
    pub park: Option<String>,
    /// Legacy compatibility read: the canonical zero-based rank within the
    /// current group. It is normalized after every move and has no meaning
    /// across groups.
    pub sort_order: Option<f64>,
    /// Execution backend: `"terminal"` (a PTY + interactive TUI) or `"acp"` (a
    /// headless adapter driven over the Agent Client Protocol). Terminal-backend
    /// and older rows read as `"terminal"`.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// The agent's own on-disk ACP session id for an `acp` session, or `null`.
    #[serde(default)]
    pub acp_session_id: Option<String>,
    /// The current ACP mode id (gating posture: `bypassPermissions`, `auto`,
    /// `acceptEdits`, `default`, `plan`), or `null` for a terminal session /
    /// before one is set.
    #[serde(default)]
    pub current_mode: Option<String>,
    /// The latest context-window usage reported by the current ACP provider, or
    /// `null` before it reports (and immediately after a provider handoff).
    #[serde(default)]
    pub usage: Option<AcpUsage>,
    /// Named launch posture selected when this session was created.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Revision of the profile whose non-secret policy was stamped at launch.
    #[serde(default)]
    pub profile_revision: i64,
    /// Stable identity of the profile lifetime accepted at launch. Zero means
    /// an upgraded row whose same-name relationship could not be proven.
    #[serde(default)]
    pub profile_lifetime: i64,
    /// Immutable environment precedence accepted at launch.
    #[serde(default)]
    pub policy_strict: bool,
    /// Monotonic lifecycle/goal mutation generation used to fence handoff.
    #[serde(default)]
    pub mutation_revision: i64,
    /// Resolved launch permission posture, immutable for this session.
    #[serde(default)]
    pub launch_mode: String,
    /// Exact, source-redacted MCP capability snapshot stamped at launch.
    #[serde(default)]
    pub mcp_policy: SessionMcpPolicyView,
    /// Canonical server-resolved launch snapshot. Older sessions created before
    /// the launch-composition contract expose `null`.
    #[serde(default)]
    pub resolved_launch: Option<ResolvedLaunchView>,
    /// The session's one canonical, operator-controlled fleet location.
    #[serde(default)]
    pub placement: Option<SessionPlacementView>,
    pub branch: BranchView,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct TitleGenerationView {
    pub enabled: bool,
    /// `idle`, `running`, `generated`, `protected`, `disabled`, `unavailable`,
    /// `stale`, or `failed`.
    pub status: String,
}

impl Default for TitleGenerationView {
    fn default() -> Self {
        Self {
            enabled: true,
            status: "idle".to_string(),
        }
    }
}

fn default_title_provenance() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ResumptionEvidenceView {
    /// `conversation` or `artifact`.
    pub kind: String,
    pub label: String,
    pub href: String,
    /// Source-stable history cursor or immutable artifact id/revision cursor.
    pub cursor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ResumptionCueView {
    /// `generated`, `generating`, `due`, `not_due`, `disabled`, or
    /// `unavailable`.
    pub status: String,
    #[serde(default)]
    pub source_cursor: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub evidence: Vec<ResumptionEvidenceView>,
}

wire_enum!(SessionSearchStatus {
    Created => "created",
    Running => "running",
    Orphaned => "orphaned",
    Done => "done",
    Error => "error",
    Archived => "archived",
});

wire_enum!(SessionSearchAttention {
    Needs => "needs",
    Ok => "ok",
    Attention => "attention",
    Blocked => "blocked",
});

// Viewer-relative creator scopes for fleet indexes. `ops` is the durable
// automation class, independent of where an operator later moves the row.
wire_enum!(SessionCreatorFilter {
    Mine => "mine",
    Ops => "ops",
    MineAndOps => "mine-and-ops",
    OtherUsers => "other-users",
});

/// Typed filters for `sessions.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchSessionsOptions {
    #[serde(default, rename = "q")]
    pub query: String,
    #[serde(default)]
    pub history: bool,
    #[serde(default)]
    pub archived_only: bool,
    #[serde(default)]
    pub status: Option<SessionSearchStatus>,
    #[serde(default)]
    pub attention: Option<SessionSearchAttention>,
    #[serde(default)]
    pub creator: Option<SessionCreatorFilter>,
    /// Include automation-class sessions. `None` takes the operation's default,
    /// which is to include them; `loom ps` passes `Some(false)`.
    #[serde(default)]
    pub automation: Option<bool>,
    /// Include engine-managed warm sessions. Human credentials only.
    #[serde(default)]
    pub managed: bool,
}

/// One session's canonical position in the shared Spaces → Groups layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionPlacementView {
    pub session_id: String,
    pub group_id: String,
    pub group_name: String,
    pub group_system_key: Option<String>,
    pub space_id: String,
    pub space_name: String,
    pub rank: i64,
}

/// An ordered, flat group inside one session space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionGroupView {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub rank: i64,
    pub system_key: Option<String>,
    /// Viewer-specific disclosure preference; membership/order remain shared.
    pub collapsed: bool,
    /// Canonically ordered session ids, including archived rows. Fleet views
    /// decide whether to project active work or History.
    pub session_ids: Vec<String>,
}

/// A top-level shared fleet space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionSpaceView {
    pub id: String,
    pub name: String,
    pub rank: i64,
    pub system_key: Option<String>,
    pub groups: Vec<SessionGroupView>,
}

/// One configurable default-placement selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionPlacementDefaultView {
    pub selector_kind: SessionPlacementSelectorKind,
    pub selector_value: String,
    pub group_id: String,
}

/// Complete shared session layout at one optimistic-concurrency revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionLayoutView {
    pub revision: i64,
    pub spaces: Vec<SessionSpaceView>,
    pub defaults: Vec<SessionPlacementDefaultView>,
}

/// The kind of one normalized history record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryKind {
    Message,
    Reasoning,
    ToolCall,
    ToolResult,
    Context,
    Event,
    Image,
}

impl HistoryKind {
    /// The wire spelling, matching `HistoryRecordView::kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Context => "context",
            Self::Event => "event",
            Self::Image => "image",
        }
    }
}

/// One provider-neutral conversation record returned by the session history
/// API. Optional fields are capability claims, not placeholders: notably,
/// `tool_input` is absent when the source protocol did not provide invocation
/// arguments (ACP currently provides only tool title/status/content/locations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryRecordView {
    /// Opaque, source-stable position used as the exclusive paging cursor.
    pub cursor: String,
    /// `message`, `reasoning`, `tool_call`, `tool_result`, `context`, `event`,
    /// or `image`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<HistoryLocationView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// A source-provided file location attached to a normalized history record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryLocationView {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// One newest-tail page of normalized session history. Records are returned in
/// chronological display order; pass `older_cursor` as `before` to continue
/// backward. The same envelope is used by literal search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryPageView {
    /// Normalizer/source label (`acp`, `claude`, `codex`, ...).
    pub source: String,
    pub records: Vec<HistoryRecordView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<String>,
}

/// Cumulative session cost optionally reported alongside ACP context usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AcpCost {
    pub amount: f64,
    pub currency: String,
}

/// Current model-context utilization for an ACP session. This is context-window
/// state, not a provider account/rate-limit quota.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AcpUsage {
    pub used: u64,
    pub size: u64,
    #[serde(default)]
    pub cost: Option<AcpCost>,
}

/// A GitHub issue association carried by a session's explicit work item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct GithubIssueRef {
    pub repo: String,
    pub number: i64,
}

fn default_protocol() -> String {
    "terminal".to_string()
}

fn default_origin() -> String {
    "user".to_string()
}

fn default_profile() -> String {
    "default".to_string()
}

fn default_profile_lifetime() -> i64 {
    1
}

fn default_prelude() -> String {
    "weaver".to_string()
}

/// A reusable, named session launch template. It is concretized into an
/// immutable `ResolvedLaunchView` for each accepted launch/handoff. Secret
/// environment values are excluded; `env` contains metadata only.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProfileView {
    pub name: String,
    pub description: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub protocol: String,
    pub mode: String,
    pub class: String,
    pub strict: bool,
    pub env_clear: bool,
    pub ambient_allowlist: Vec<String>,
    pub idle_archive_secs: Option<i64>,
    pub max_concurrent: i64,
    pub turn_budget: Option<i64>,
    #[serde(default = "default_prelude")]
    pub prelude: String,
    /// Organization-owned instructions appended to this profile's opening
    /// prompt for every launch origin.
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub restricted: bool,
    #[serde(default)]
    pub github_repositories: Vec<String>,
    #[serde(default)]
    pub runtime_permissions: Vec<String>,
    #[serde(default)]
    pub mcp_access: McpAccess,
    /// Servers predating profile lifetimes expose only the original selectable
    /// lifetime, so a newer typed client can safely interpret omission as 1.
    #[serde(default = "default_profile_lifetime")]
    pub lifetime: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub env: Vec<ProfileEnvView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProfileEnvView {
    pub name: String,
    /// `literal` or `gcp_secret`. The value itself is never returned.
    pub source: String,
    #[serde(default)]
    pub secret_ref: Option<String>,
    pub updated_at: String,
}

/// One trusted MCP adapter Loom can launch.  This is deliberately provider
/// neutral: clients select a capability set, while an agent runtime translates
/// its tools into that provider's permission vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpAdapterView {
    pub name: String,
    pub description: String,
    pub server_name: String,
}

/// An inspectable, content-addressed collection of MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpCapabilitySetView {
    pub name: String,
    pub group: String,
    pub version: String,
    pub digest: String,
    pub description: String,
    pub adapter: String,
    pub tools: Vec<String>,
    /// Canonical replacement for a compatibility-only capability identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_by: Option<String>,
}

/// The trusted MCP registry exposed to operators and the settings UI.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpRegistryView {
    pub adapters: Vec<McpAdapterView>,
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<CustomMcpView>,
}

/// Provider-neutral MCP selection carried by a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpAccess {
    /// `none`, `all`, or `groups`.
    pub mode: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl Default for McpAccess {
    fn default() -> Self {
        Self {
            mode: "none".to_string(),
            groups: Vec::new(),
        }
    }
}

/// Exact MCP registry content stamped onto a launched session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpPolicySnapshot {
    pub selection: McpAccess,
    #[serde(default)]
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<CustomMcpSnapshot>,
}

/// Source-redacted MCP audit policy returned on ordinary session views.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionMcpPolicyView {
    pub selection: McpAccess,
    #[serde(default)]
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<SessionCustomMcpView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionCustomMcpView {
    pub identity: String,
    pub group: String,
    pub revision: i64,
    pub digest: String,
    pub server_name: String,
    pub tools: Vec<String>,
}

impl From<&McpPolicySnapshot> for SessionMcpPolicyView {
    fn from(snapshot: &McpPolicySnapshot) -> Self {
        Self {
            selection: snapshot.selection.clone(),
            capability_sets: snapshot.capability_sets.clone(),
            custom_servers: snapshot
                .custom_servers
                .iter()
                .map(|server| SessionCustomMcpView {
                    identity: server.identity.clone(),
                    group: server.group.clone(),
                    revision: server.revision,
                    digest: server.digest.clone(),
                    server_name: server.server_name.clone(),
                    tools: server.tools.clone(),
                })
                .collect(),
        }
    }
}

/// Exact executable custom MCP revision stamped onto a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CustomMcpSnapshot {
    pub identity: String,
    pub group: String,
    pub revision: i64,
    pub digest: String,
    pub server_name: String,
    pub tools: Vec<String>,
    /// Source is part of the immutable recovery snapshot. It is operator-authored
    /// code, never a credential, and is not exposed in ordinary session views.
    pub source: String,
}

/// Body for creating or updating an operator-authored MCP server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CustomMcpReq {
    pub identity: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub source: String,
    #[serde(default)]
    pub test_source: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Latest custom MCP definition and validation result.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CustomMcpView {
    pub identity: String,
    pub group: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
    pub revision: i64,
    pub digest: String,
    pub source: String,
    pub test_source: String,
    pub tools: Vec<String>,
    pub validation_state: String,
    pub validation_message: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Response from the scalar `mcps.custom.delete` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct CustomMcpDeleteResult {
    pub deleted: bool,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct McpServerProcessView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Fully resolved non-secret profile policy without launching a session.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EffectiveProfileView {
    pub profile: ProfileView,
    pub mcp_policy: McpPolicySnapshot,
    pub runtime_permissions: Vec<String>,
    pub mcp_servers: Vec<McpServerProcessView>,
}

/// Response from the scalar `profiles.delete` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ProfileDeleteResult {
    pub deleted: bool,
    pub name: String,
}

/// Fields a caller may layer over a named profile for one launch. Presence is
/// significant: an omitted (or blank agent) field inherits while an explicit
/// empty model or effort selects the agent's own default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchOverrides {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
}

/// Canonical profile-template selection accepted by launch preview, session
/// create, handoff, and profile clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchSelection {
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub overrides: LaunchOverrides,
}

impl Default for LaunchSelection {
    fn default() -> Self {
        Self {
            profile: default_profile(),
            overrides: LaunchOverrides::default(),
        }
    }
}

/// Provenance for every concrete runtime selector in a resolved launch.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchProvenanceView {
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub protocol: String,
    pub mode: String,
    pub class: String,
    #[serde(default)]
    pub idle_archive_secs: String,
    #[serde(default)]
    pub turn_budget: String,
}

/// Capacity observed while resolving a launch. The repository launch gate
/// rechecks it immediately before provisioning, so this is an honest preview,
/// not an admission reservation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchCapacityView {
    pub active: i64,
    pub maximum: Option<i64>,
    pub available: Option<i64>,
    pub allowed: bool,
}

/// Source-redacted security and lifecycle policy that will be stamped on the
/// session. Environment values and custom MCP source are deliberately absent.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResolvedLaunchPolicyView {
    pub strict: bool,
    pub restricted: bool,
    pub env_clear: bool,
    pub environment: Vec<ProfileEnvView>,
    pub ambient_allowlist: Vec<String>,
    pub idle_archive_secs: Option<i64>,
    pub turn_budget: Option<i64>,
    pub prelude: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub github_repositories: Vec<String>,
    pub runtime_permissions: Vec<String>,
    pub mcp_policy: SessionMcpPolicyView,
}

/// Concrete source-redacted immutable launch snapshot returned by preview and
/// exposed on the created session (or replacement handoff runtime).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResolvedLaunchView {
    pub selection: LaunchSelection,
    #[serde(default)]
    pub profile_lifetime: i64,
    pub profile_revision: i64,
    pub resolver_revision: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub protocol: String,
    pub mode: String,
    pub class: String,
    pub locked_fields: Vec<String>,
    pub provenance: LaunchProvenanceView,
    pub capacity: LaunchCapacityView,
    pub policy: ResolvedLaunchPolicyView,
    pub valid: bool,
    pub errors: Vec<String>,
}

/// Atomic environment composition for a cloned profile. Inherited values are
/// copied server-side; literal values and secret references are write-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CloneProfileEnvironmentReq {
    #[serde(default)]
    pub inherit: bool,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub set: Vec<ProfileEnvMutationReq>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProfileEnvMutationReq {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

/// Result of archiving a custom channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelArchiveResult {
    pub archived: bool,
}

/// One file in a session's Scratch directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScratchFileView {
    pub name: String,
    pub bytes: u64,
}

/// Result of writing a Scratch file: the accepted name, its size, and the
/// worktree-relative path the session sees it at.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScratchWriteResult {
    pub name: String,
    pub bytes: u64,
    pub path: String,
}

/// Result of deleting a Scratch file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScratchDeleteResult {
    pub name: String,
    pub deleted: bool,
}

/// Shared upload limits for launch-time and live-session Scratch attachments:
/// 20 files, 25 MiB each, 50 MiB decoded total. `.gitignore` is reserved.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScratchLimitsView {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_name_bytes: usize,
}

/// A short-lived GitHub App installation token brokered for one session.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GithubTokenView {
    pub token: String,
}

/// One explicit repository grant layered onto a session's launch-time GitHub
/// policy. GitHub App credentials currently expose one reviewed write policy;
/// `none` is accepted only as the mutation that revokes a grant.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionGithubAccessView {
    pub repository: String,
    pub mode: String,
    pub granted_by: String,
    pub granted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetSessionGithubAccessReq {
    pub repository: String,
    pub mode: String,
}

/// Durable request for a human to expand one live session's external access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct PermissionRequestView {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub repository: String,
    pub mode: String,
    pub reason: String,
    pub state: String,
    pub requested_by: String,
    pub requested_at: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub decision_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecidePermissionRequestReq {
    /// `approve` or `deny`.
    pub decision: String,
    #[serde(default)]
    pub reason: String,
}

/// Current Loom operation grants and external repository scope for a session.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EffectivePermissionsView {
    pub session_id: String,
    pub actor: String,
    pub operations: Vec<String>,
    pub github_repositories: Vec<String>,
    pub pending_requests: Vec<PermissionRequestView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AutomationTokenView {
    pub token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FederateReq {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FederationReq {
    /// Stable operator-owned identity used for idempotent reconciliation.
    pub name: String,
    #[serde(default = "github_provider")]
    pub provider: String,
    #[serde(default = "github_oidc_issuer")]
    pub issuer: String,
    pub audience: String,
    /// Exact numeric OIDC subject for Google workload identities.
    #[serde(default)]
    pub subject: Option<String>,
    /// Exact verified Google service-account email.
    #[serde(default)]
    pub service_account: Option<String>,
    /// Stable, bounded audit label copied into Loom automation credentials.
    pub service_tag: String,
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub event_name: Option<String>,
    #[serde(default)]
    pub ref_pattern: Option<String>,
    pub profiles: Vec<String>,
}

fn github_provider() -> String {
    "github".to_string()
}

fn github_oidc_issuer() -> String {
    "https://token.actions.githubusercontent.com".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FederationView {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub issuer: String,
    pub audience: String,
    pub subject: Option<String>,
    pub service_account: Option<String>,
    pub service_tag: String,
    pub repository_id: Option<String>,
    pub workflow_ref: Option<String>,
    pub event_name: Option<String>,
    pub ref_pattern: Option<String>,
    pub profiles: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeploymentProfileEnvReq {
    pub name: String,
    /// Omit both fields to preserve an existing write-only value by name.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

/// One named profile and its authoritative write-only environment declaration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeploymentProfileReq {
    pub profile: crate::operations::profiles::update::Input,
    #[serde(default)]
    pub env: Vec<DeploymentProfileEnvReq>,
}

/// A scalar setting value in a JSON or YAML deployment manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum DeploymentSettingValue {
    String(String),
    Int(i64),
    Bool(bool),
}

impl DeploymentSettingValue {
    pub fn stored(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeploymentView {
    pub settings: Vec<SettingView>,
    pub profiles: Vec<ProfileView>,
    pub federations: Vec<FederationView>,
}

/// One Slack thread, as an automation caller names it. `channel` is a Slack
/// channel id (`C…`/`G…`/`D…`, never a `#name`) and `thread_ts` the message `ts`
/// of the thread's root. The workspace is loom's own — a caller cannot address
/// another team — and the bot token stays server-side, so this is a destination
/// request, not a capability the caller holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SlackThreadRef {
    pub channel: String,
    pub thread_ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RestrictedGithubToolView {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunView {
    pub id: String,
    pub actor_subject: String,
    pub source: String,
    pub watch_id: Option<String>,
    pub service_tag: String,
    pub profile: String,
    pub idempotency_key: String,
    pub channel: Option<String>,
    pub session_id: String,
    pub status: String,
    pub outcome: Option<String>,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One detached background task's lifecycle, as `GET /api/tasks` exposes it —
/// currently the GitHub `@loom` trigger launches, which run off the webhook
/// request so a slow clone can't blow GitHub's delivery timeout. Human-only
/// self-service debugging (Settings → Diagnostics), same as the log endpoints:
/// a task label names a repo/issue an operator can act on.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskView {
    pub id: u64,
    /// A coarse category, e.g. `github-trigger`.
    pub kind: String,
    /// A human label, e.g. `marin-community/marin#6823 (@rjpower)`.
    pub label: String,
    /// `running` | `done` | `error`.
    pub state: String,
    /// Outcome detail: a session id, `forwarded`, or an error message.
    pub detail: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// One migration stream's observed and expected state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct MigrationStreamView {
    pub stream: String,
    pub current: i64,
    pub expected: i64,
    pub applied: i64,
    pub ready: bool,
}

/// Public readiness response. Liveness remains the process-only `/api/health`;
/// this shape proves that the database and both migration streams are usable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ReadinessView {
    pub status: String,
    pub database: bool,
    pub migrations: Vec<MigrationStreamView>,
    /// Optional facilities may report degraded here without taking the API out
    /// of service. Empty until remote runner pools exist.
    pub degraded: Vec<String>,
}

/// A session count across every bounded control-plane dimension available in
/// the current schema. `runner_pool` is `local` until runner pools land.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticSessionCount {
    pub status: String,
    pub class: String,
    pub profile: String,
    pub protocol: String,
    pub runner_pool: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticProfileCapacity {
    pub profile: String,
    pub revision: i64,
    pub active: i64,
    /// `None` means unlimited (`max_concurrent = 0`).
    pub maximum: Option<i64>,
    pub available: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticRunCount {
    pub status: String,
    pub source: String,
    pub service_tag: String,
    pub profile: String,
    pub count: i64,
}

/// A redacted recent failed run. Deliberately excludes actor, idempotency key,
/// session id, request body, and raw failure summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticRunFailure {
    pub source: String,
    pub profile: String,
    pub outcome: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticRunSummary {
    pub counts: Vec<DiagnosticRunCount>,
    pub stale_creating: i64,
    pub recent_failures: Vec<DiagnosticRunFailure>,
}

/// Aggregated orphan/error inventory. No session, branch, path, principal, or
/// error text crosses this diagnostics boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticProblemSummary {
    pub status: String,
    pub class: String,
    pub profile: String,
    pub protocol: String,
    pub runner_pool: String,
    pub count: i64,
    pub latest_activity_at: Option<String>,
}

/// Non-secret federation mapping metadata useful for verifying deployment
/// reconciliation. This never includes a bearer/OIDC token or signing key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticFederation {
    pub name: String,
    pub provider: String,
    pub audience: String,
    pub service_tag: String,
    pub profiles: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Human-readable operational snapshot returned by `/api/diagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DiagnosticsView {
    pub sessions: Vec<DiagnosticSessionCount>,
    pub profiles: Vec<DiagnosticProfileCapacity>,
    pub automation_runs: DiagnosticRunSummary,
    pub problems: Vec<DiagnosticProblemSummary>,
    pub migrations: Vec<MigrationStreamView>,
    pub federations: Vec<DiagnosticFederation>,
}

fn default_class() -> String {
    "interactive".to_string()
}

/// Issue as the API exposes it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueView {
    pub id: i64,
    pub repo_root: String,
    pub github_repo: Option<String>,
    /// Branch the issue was created from (provenance).
    pub source_branch: Option<String>,
    /// Branch currently working it; `null` is the unclaimed repo backlog.
    pub claimed_branch: Option<String>,
    pub title: String,
    pub body: String,
    pub status: String,
    pub github_issue: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// Free-form `(key, value)` labels on the issue, rendered as quiet pills.
    /// Empty when the issue carries none. Unlike branch tags these never carry
    /// the loud `attention`/`triage` ladder.
    pub tags: Vec<TagView>,
    /// Live state of the linked GitHub thread, fetched at read time by the
    /// single-issue endpoint when the issue carries a `github_repo` +
    /// `github_issue` link. Absent on list endpoints (no fan-out of GitHub
    /// calls) and when the fetch fails — the ledger fields above still stand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_state: Option<GithubThreadState>,
}

/// One initial tag supplied while creating an issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueTagInput {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub by: Option<String>,
}

/// One command validated and applied atomically to every requested issue.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IssueAction {
    Close,
    Reopen,
    Tag {
        key: String,
        value: String,
        #[serde(default)]
        note: String,
        #[serde(default)]
        by: Option<String>,
    },
    Untag {
        key: String,
    },
    Delete,
}

/// One ID or precondition reported in an atomic action error's `details`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueActionProblem {
    pub id: i64,
    /// Stable machine-readable category such as `not_found`, `invalid_state`,
    /// or `missing_tag`.
    pub code: String,
    pub error: String,
}

/// Aggregate outcome from a successful atomic `POST /api/issues/actions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueActionsResult {
    /// Updated issue views for close, reopen, tag, and untag.
    pub issues: Vec<IssueView>,
    /// Deleted IDs for delete. Empty for every other action.
    pub deleted_ids: Vec<i64>,
}

/// Response from the scalar `DELETE /api/issues/{id}` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct DeleteIssueResult {
    pub deleted: bool,
}

/// The minimal live snapshot of a GitHub thread `loom issues get` renders
/// beside the weaver ledger: enough to notice "this was closed / re-titled
/// while I worked". An agent that needs the discussion reads it with `gh`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GithubThreadState {
    /// `open` | `closed`.
    pub state: String,
    pub title: String,
    /// ISO time of the thread's last touch, as GitHub reports it.
    pub updated_at: String,
}

impl IssueView {
    /// Build the wire view from an [`Issue`] and the tags gathered for it.
    pub fn from_parts(i: Issue, tags: &[Tag]) -> Self {
        IssueView {
            id: i.id,
            repo_root: i.repo_root,
            github_repo: i.github_repo,
            source_branch: i.source_branch,
            claimed_branch: i.claimed_branch,
            title: i.title,
            body: i.body,
            status: i.status,
            github_issue: i.github_issue,
            created_at: i.created_at,
            updated_at: i.updated_at,
            closed_at: i.closed_at,
            tags: tags.iter().map(TagView::from).collect(),
            github_state: None,
        }
    }
}

impl From<Issue> for IssueView {
    /// Convenience for call sites that don't surface tags (the tag list is left
    /// empty). Tag-aware endpoints use [`IssueView::from_parts`].
    fn from(i: Issue) -> Self {
        IssueView::from_parts(i, &[])
    }
}

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents an agent (or the user) writes to
// weaver. The envelope, a version row, and the full view (content + projected
// references). The projection backs both the SPA chips and `loom artifacts
// show`. See docs/artifacts.md.
// ---------------------------------------------------------------------------

/// An artifact envelope as the API exposes it: identity, kind, title, scope, and
/// its latest revision number.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactMeta {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub title: String,
    /// The branch that owns it, or `null` for a repo-shared artifact.
    pub branch_id: Option<String>,
    /// The latest revision number.
    pub rev: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One revision of an artifact (metadata only — the version picker lists these;
/// content is fetched per-rev through the artifact GET with `?rev=`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactVersion {
    pub rev: i64,
    /// `agent` | `user` — who wrote this revision.
    pub author: String,
    pub created_at: String,
}

/// The live status of one issue referenced from an artifact — what the renderer
/// stamps into a `#N` chip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueRefStatus {
    pub id: i64,
    pub title: String,
    /// `open` | `closed`.
    pub status: String,
    /// The branch working it; `null` is the unclaimed backlog.
    pub claimed_branch: Option<String>,
}

/// The projected reference map an artifact's content names. Keyed by id-as-string
/// so it round-trips cleanly through JSON object keys. v1 projects issues; the
/// `artifact:`/`session:` reference kinds are reserved for later probes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactRefs {
    /// `{"<issue id>": { id, title, status, claimed_branch }}` for every `#N`
    /// the content references.
    #[serde(default)]
    pub issues: std::collections::BTreeMap<String, IssueRefStatus>,
}

/// The full artifact view returned by the artifact GET/PUT: the envelope, the
/// content of the selected (default latest) revision, the version list for a
/// picker, and the projected reference map.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactView {
    pub meta: ArtifactMeta,
    /// Raw content of the selected revision — the dashboard renders and edits it.
    pub content: String,
    /// Every revision, newest first, for the version picker.
    pub versions: Vec<ArtifactVersion>,
    /// References found in the content, resolved against the live ledger.
    pub refs: ArtifactRefs,
}

/// Response from `artifacts.delete`: confirms the artifact and its complete
/// revision and discussion history were removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ArtifactDeleteResult {
    pub deleted: bool,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Discussion — resolvable, stand-off comment threads anchored to a quoted span
// of an artifact. The anchor is a W3C-style text-quote selector (quote +
// surrounding context), not a char offset, so it survives edits made
// elsewhere in the document. See `weaver_core::discussion` and
// docs/artifacts.md.
// ---------------------------------------------------------------------------

/// A thread's anchor: the quoted span plus a little surrounding context for
/// disambiguation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnchorDto {
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
}

/// One reply in a thread, as the API exposes it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommentDto {
    pub seq: i64,
    /// `agent` | `user`.
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A discussion thread on an artifact span: its anchor, status, and comments
/// (oldest first), as the GET/POST thread endpoints expose it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ThreadDto {
    pub id: i64,
    /// The artifact revision the anchor was taken from.
    pub base_rev: i64,
    pub anchor: AnchorDto,
    /// `open` | `resolved` | `orphaned`.
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub comments: Vec<CommentDto>,
}

/// Body for `POST /api/sessions/{id}/artifacts/{name}/threads`: open a new
/// thread anchored to a quoted span, seeded with its first comment.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewThreadBody {
    pub base_rev: i64,
    pub anchor: AnchorDto,
    pub body: String,
}

/// Body for `POST /api/sessions/{id}/artifacts/{name}/threads/{tid}/comments`:
/// append a reply to an existing thread.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewCommentBody {
    pub body: String,
}

// ---------------------------------------------------------------------------
// Reviews — creator-private draft feedback over versioned artifacts. The
// generic subject/anchor shapes are shared with the future changes viewer.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSubjectKindDto {
    Artifact,
    Changes,
}

impl std::str::FromStr for ReviewSubjectKindDto {
    type Err = String;

    fn from_str(kind: &str) -> Result<Self, Self::Err> {
        match kind.trim() {
            "artifact" => Ok(Self::Artifact),
            "changes" => Ok(Self::Changes),
            other => Err(format!("unknown review subject kind `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAnchorKindDto {
    Text,
    Change,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewSubjectDto {
    pub kind: ReviewSubjectKindDto,
    /// Stable internal artifact envelope id.
    pub id: String,
    /// Stable public subject key: the artifact name accepted by list/create.
    pub key: String,
    pub label: String,
    pub version: String,
    pub current_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTextAnchorDto {
    pub quote: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub block_index: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSideDto {
    Old,
    New,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeAnchorDto {
    pub path: ChangePathDto,
    pub side: ChangeSideDto,
    pub start_line: u32,
    pub end_line: u32,
    pub hunk_header: String,
    #[serde(default)]
    pub context_before: Vec<String>,
    pub selected: Vec<String>,
    #[serde(default)]
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ReviewAnchorDto {
    Text(ArtifactTextAnchorDto),
    Change(ChangeAnchorDto),
}

impl From<ArtifactTextAnchorDto> for ReviewAnchorDto {
    fn from(value: ArtifactTextAnchorDto) -> Self {
        Self::Text(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewCommentDto {
    pub id: i64,
    pub subject_version: String,
    pub anchor_kind: ReviewAnchorKindDto,
    pub anchor: ReviewAnchorDto,
    pub body: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewDto {
    pub id: i64,
    pub session_id: String,
    pub subject: ReviewSubjectDto,
    pub status: String,
    pub summary: String,
    /// Monotonic optimistic revision for the editable draft envelope.
    pub draft_revision: i64,
    /// Server-authoritative exact conversation payload preview.
    pub message: String,
    pub created_by: String,
    pub outdated: bool,
    pub acknowledged_outdated: bool,
    pub delivery_state: String,
    pub delivery_error: Option<String>,
    pub delivery_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub submitted_at: Option<String>,
    pub comments: Vec<ReviewCommentDto>,
    #[serde(default)]
    pub legacy: bool,
}

// ---------------------------------------------------------------------------
// Changes — one bounded, typed snapshot of the session worktree relative to
// its real branch base.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeFileStatusDto {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSourceDto {
    Committed,
    Staged,
    Unstaged,
    Untracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeContentDto {
    Text,
    Binary,
    Oversize,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeLineKindDto {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeLineDto {
    pub kind: ChangeLineKindDto,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeHunkDto {
    pub header: String,
    pub lines: Vec<ChangeLineDto>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeFileDto {
    pub status: ChangeFileStatusDto,
    pub path: ChangePathDto,
    pub old_path: Option<ChangePathDto>,
    pub sources: Vec<ChangeSourceDto>,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub content: ChangeContentDto,
    pub hunks: Vec<ChangeHunkDto>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangePathDto {
    /// URL-safe base64 of the exact repo-relative Git path bytes.
    pub bytes: String,
    /// Escaped, control-free display form; never used as identity.
    pub display: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeBaseUnavailableReasonDto {
    UnbornHead,
    MissingBase,
    NoMergeBase,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChangeBaseDto {
    Available {
        reference: String,
        oid: String,
    },
    Unavailable {
        reference: String,
        reason: ChangeBaseUnavailableReasonDto,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeTotalsDto {
    pub files: u32,
    pub additions: u32,
    pub deletions: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeLimitsDto {
    pub max_files: u32,
    pub max_hunks_per_file: u32,
    pub max_lines_per_file: u32,
    pub max_total_lines: u32,
    pub max_line_bytes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChangeSetDto {
    pub version: Option<String>,
    pub base: ChangeBaseDto,
    pub head_oid: Option<String>,
    pub totals: ChangeTotalsDto,
    pub files: Vec<ChangeFileDto>,
    pub truncated: bool,
    pub limits: ChangeLimitsDto,
}

/// One watch, as the API exposes it. The JSON-bearing columns
/// (`trigger`, `scope`, `params`) are returned as **parsed** structured JSON so
/// a UI never re-parses strings; `capabilities` is a real array; the rest is the
/// stored definition plus its schedule bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// The event-match predicate, parsed: `{cron|every|event|level|repo}`.
    pub trigger: Value,
    /// The fleet query a round surveys, parsed: `{attention?, repo?}`.
    pub scope: Value,
    /// `builtin:<name>` for a stock program, or an absolute path under
    /// `~/.weaver/watches/` for a custom one.
    pub program: String,
    /// Stock-program parameters (e.g. the judgement `prompt`), parsed.
    pub params: Value,
    /// The granted capability set (the intervention ladder). `observe` is
    /// implicit; the rest are explicit grants.
    pub capabilities: Vec<String>,
    /// Automation-safe launch profile used for agent judgements and warm
    /// sessions.
    pub profile: String,
    pub model: String,
    pub effort: String,
    pub cooldown_secs: i64,
    /// Warm mode (`params.warm`): the engine keeps one long-lived, fleet-hidden
    /// session for this watch so it has across-round memory.
    pub warm: bool,
    /// The id of that warm session once the engine has created it, else `null`.
    /// Its live terminal is reachable from the watch's detail page (the
    /// session is hidden from the fleet listing).
    pub warm_session_id: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    /// The one-shot dynamic re-trigger time a round armed (`wake_in`), or `null`.
    /// Distinct from `next_run_at` (the cron cadence): a self-scheduled backoff
    /// recheck a watch set for itself.
    pub wake_at: Option<String>,
    /// The program's lookaside state, parsed — its scratch memory carried across
    /// rounds (e.g. a backoff watcher's per-session attempt counts). `{}` when
    /// the program keeps none.
    pub state: Value,
    /// The most recent round's outcome (`ok|noop|skipped|error`), or `null` if
    /// it has never run — the at-a-glance health a list view shows.
    pub last_outcome: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl WatchView {
    /// Build the wire view from a watch plus the most recent round's
    /// outcome (the server reads that from the run history). The JSON columns
    /// are parsed here via the domain accessors.
    pub fn from_parts(o: &Watch, last_outcome: Option<String>) -> Self {
        Self {
            id: o.id.clone(),
            name: o.name.clone(),
            enabled: o.enabled,
            trigger: serde_json::to_value(o.trigger()).unwrap_or(Value::Null),
            scope: serde_json::to_value(o.scope()).unwrap_or(Value::Null),
            program: o.program.clone(),
            params: o.params(),
            capabilities: o.capabilities(),
            profile: o.profile.clone(),
            model: o.model.clone(),
            effort: o.effort.clone(),
            cooldown_secs: o.cooldown_secs,
            warm: o.warm(),
            warm_session_id: o.warm_session_id.clone(),
            last_run_at: o.last_run_at.clone(),
            next_run_at: o.next_run_at.clone(),
            wake_at: o.wake_at.clone(),
            state: o.state(),
            last_outcome,
            created_at: o.created_at.clone(),
            updated_at: o.updated_at.clone(),
        }
    }
}

/// One round in a watch's history (the audit trail), with `actions`
/// parsed back into JSON for a UI to render. The `stdout`/`stderr`/`exit_code`/
/// `duration_ms` fields are the captured execution log — what the script printed
/// and returned — surfaced so a run page shows exactly what happened.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchRunView {
    pub id: i64,
    pub trigger_reason: String,
    /// The normalized event that woke the round (`cron` / `manual` / e.g.
    /// `pr.merged`).
    pub trigger_event: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub summary: String,
    /// The JSON array of marks / nudges / would-dos the round recorded.
    pub actions: Value,
    /// A tail of the script's standard output.
    pub stdout: String,
    /// A tail of the script's standard error.
    pub stderr: String,
    /// The interpreter's exit status, or `null` when it never spawned / timed out.
    pub exit_code: Option<i64>,
    /// Wall-clock the program ran, in milliseconds.
    pub duration_ms: Option<i64>,
}

impl From<WatchRun> for WatchRunView {
    fn from(r: WatchRun) -> Self {
        Self {
            id: r.id,
            trigger_reason: r.trigger_reason,
            trigger_event: r.trigger_event,
            started_at: r.started_at,
            finished_at: r.finished_at,
            outcome: r.outcome,
            summary: r.summary,
            actions: serde_json::from_str(&r.actions).unwrap_or(Value::Null),
            stdout: r.stdout,
            stderr: r.stderr,
            exit_code: r.exit_code,
            duration_ms: r.duration_ms,
        }
    }
}

/// One **program** a watch can run, as `GET /api/watches/programs`
/// exposes it. Builtin programs are Python scripts that ship inside the loom
/// binary; the embedded source is returned for a read-only view in the panel.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProgramView {
    /// The reference a watch's `program` field names it by, e.g.
    /// `builtin:status` or `builtin:archive-merged`.
    pub program: String,
    pub title: String,
    pub description: String,
    /// The program's embedded Python source. Read-only — it ships with the
    /// binary.
    pub source: String,
    /// Suggested starting config for a new watch running this program:
    /// `{trigger, scope, params, capabilities}` — what a create form prefills.
    pub defaults: Value,
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

pub const CHANNEL_DEFAULT_MESSAGE_KIND: &str = "message";
pub const CHANNEL_DEFAULT_URGENCY: &str = "normal";
pub const CHANNEL_DEFAULT_SUBSCRIPTION_MODE: &str = "observe";
pub const CHANNEL_MESSAGE_LIMIT_MAX: usize = 500;
pub const CHANNEL_IDEMPOTENCY_KEY_MAX_LEN: usize = 255;
// `branches.slack.reply` and `channels.messages.create` spell these bounds as
// schemars literals, which cannot reference a constant.
const _: () = assert!(CHANNEL_IDEMPOTENCY_KEY_MAX_LEN == 255);
const _: () = assert!(CHANNEL_MESSAGE_LIMIT_MAX == 500);
pub const CHANNEL_SLACK_ORIGIN_BINDING_ID: &str = "slack:origin";

pub fn channel_session_binding_id(session_id: &str) -> String {
    format!("session:{session_id}")
}

/// One durable communication context. A session channel uses its owning
/// session id as `id`; custom channels have an independent id.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelView {
    pub id: String,
    pub kind: String,
    pub repo_root: String,
    pub branch_id: Option<String>,
    pub session_id: Option<String>,
    pub name: String,
    pub topic: String,
    pub state: String,
    pub created_by_kind: String,
    pub created_by: String,
    pub created_at: String,
    pub archived_at: Option<String>,
    #[serde(default)]
    pub unread_count: i64,
    #[serde(default)]
    pub unread_urgent_count: i64,
    #[serde(default)]
    pub last_message: Option<ChannelMessageView>,
    /// This channel's server-owned delivery bindings. The old MCP `get`/`list`
    /// tools fetched these with a second call and merged them in by hand;
    /// they are part of the response itself now, so REST, the CLI, and MCP
    /// all see the same shape.
    #[serde(default)]
    pub bindings: Vec<ChannelBindingView>,
}

/// One server-owned destination bound to a durable channel. Agents address the
/// Loom channel; the daemon owns provider coordinates and reports delivery per
/// binding without exposing credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ChannelBindingView {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub target_session_id: Option<String>,
}

/// One append-only item in a channel's monotonically sequenced history.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelMessageView {
    pub id: String,
    pub channel_id: String,
    pub seq: i64,
    pub kind: String,
    pub urgency: String,
    pub author_kind: String,
    pub author_id: String,
    pub body: String,
    pub payload: Value,
    pub reply_to: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub deliveries: Vec<ChannelDeliveryView>,
}

/// Attempt and outcome for delivery of one channel message to one binding.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelDeliveryView {
    /// Stable identity within the channel, for example `session:<id>` or
    /// `slack:origin`.
    pub binding_id: String,
    /// `session`, `slack_thread`, or a future transport kind.
    pub binding_kind: String,
    #[serde(default)]
    pub target_session_id: Option<String>,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    pub updated_at: String,
}

/// Caller-relative bootstrap context used by in-session tools. REST resources
/// remain canonically id-addressed; this view resolves `self` once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SelfContextView {
    pub session_id: String,
    pub branch_id: String,
    /// The branch's human name. Carried alongside the id because context fields
    /// need both and confusing them is silent — see `ContextSource::BranchName`.
    #[serde(default)]
    pub branch_name: String,
    pub repo_root: String,
    pub channel_id: String,
    pub session_url: String,
    pub links: SelfContextLinks,
}

/// Where a session reads its own channel, artifacts, and session record.
///
/// Each value is an operation's path, not a per-id URL: the operand these three
/// reads take is the caller's own context, so a session credential posting an
/// empty body to any of them gets its own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SelfContextLinks {
    pub channel: String,
    pub artifacts: String,
    pub session: String,
}

/// One structured catch-up for an agent resuming a session. Consumers render
/// this for terminals or return it directly over MCP.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionCatchupView {
    pub session_id: String,
    pub branch_id: String,
    pub goal: String,
    pub attention: String,
    pub status_message: String,
    pub channel: Option<ChannelView>,
    pub artifacts: Vec<ArtifactMeta>,
    pub issues: Vec<IssueView>,
    pub recent_events: Vec<weaver_core::events::Event>,
    pub next_actions: Vec<String>,
}

/// The authenticated caller's subscription to a channel.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelSubscriptionView {
    pub channel_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub mode: String,
    pub read_seq: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Request payloads
// ---------------------------------------------------------------------------

/// One launch-time scratch file: a name plus its base64-encoded bytes. JSON
/// can't carry raw binary, so the UI reads each dropped file as base64.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScratchUpload {
    pub name: String,
    #[serde(default)]
    pub content_base64: String,
}

/// Body for `POST /api/sessions/{id}/handoff`. Canonical `selection` requires
/// both revisions from `/handoff/resolve` and stamps the target template.
/// Flattened fields remain for backward compatibility, preserving the
/// session's stamped profile/policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HandoffReq {
    /// Flattened runtime selector for backward compatibility. Canonical clients use `selection`.
    #[serde(default)]
    pub agent: String,
    /// Blank/absent uses the target runtime's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Blank/absent uses the target runtime's default.
    #[serde(default)]
    pub effort: Option<String>,
    /// ACP permission posture. Blank/absent uses the configured `agent.mode`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub selection: Option<LaunchSelection>,
    #[serde(default)]
    pub expected_profile_revision: Option<i64>,
    #[serde(default)]
    pub expected_resolver_revision: Option<String>,
}

// Reorder one space, or one group (optionally into another space).
wire_enum!(SessionLayoutItemKind {
    Space => "space",
    Group => "group",
});

/// Atomically move one or more sessions to an exact group insertion point.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveSessionsReq {
    pub session_ids: Vec<String>,
    pub destination_group_id: String,
    #[serde(default)]
    pub before_session_id: Option<String>,
    pub expected_revision: i64,
}

/// One complete group order in an atomic layout restore.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionGroupOrderReq {
    pub group_id: String,
    pub session_ids: Vec<String>,
}

wire_enum!(SessionPlacementSelectorKind {
    Origin => "origin",
    Profile => "profile",
    Watch => "watch",
});

/// One desired tag in `PUT /api/sessions/{id}/tags`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagInput {
    pub key: String,
    pub value: String,
    /// One-line reason accompanying the tag.
    #[serde(default)]
    pub note: String,
}

/// One exact `(key, value)` tag to clear in the same atomic replacement.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagMatch {
    pub key: String,
    pub value: String,
}

/// Body for `POST /api/sessions/{id}/send`: type a message into the agent pane.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SendReq {
    /// The text to type into the agent's pane.
    pub text: String,
    /// Whether to follow the text with Enter to submit it (and so trigger an
    /// agent round). Defaults to true; pass false to stage input unsubmitted.
    #[serde(default = "default_submit")]
    pub submit: bool,
    /// Who is sending (a watch name or `manual`) — recorded on the
    /// `nudge` audit event; the server defaults a missing author.
    #[serde(default)]
    pub by: Option<String>,
}

fn default_submit() -> bool {
    true
}

impl SendReq {
    /// A submitting send (the default): type `text` and press Enter.
    pub fn submit(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            submit: true,
            by: None,
        }
    }
}

/// Result of `POST /api/sessions/{id}/send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionSendResult {
    pub sent: bool,
    pub submitted: bool,
    /// Whether the prompt was queued behind an active turn. Set only for an ACP
    /// session; `null` for a terminal session, which has no queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<bool>,
    /// The turn the prompt belongs to. Set only for an ACP session; `null`
    /// for a terminal session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<i64>,
}

/// Result of `POST /api/sessions/{id}/interrupt`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionInterruptResult {
    pub interrupted: bool,
}

/// Result of `GET /api/sessions/{id}/preview`: the session's terminal pane (or,
/// for an ACP session, its recent journal) rendered as plain text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionPreviewResult {
    pub screen: String,
}

/// Body for `PUT /api/sessions/{id}/artifacts/{name}`: a user edit that appends
/// a new revision (`author: user`). `title`/`kind` update the envelope; omit
/// them to keep the current values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactWriteBody {
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Optimistic-concurrency guard: if set and it doesn't match the
    /// artifact's current latest revision, the server rejects the write with
    /// 409 instead of silently overwriting a newer edit. Omitted (the
    /// default) force-writes as before — backward compatible.
    #[serde(default)]
    pub base_rev: Option<i64>,
}

wire_enum!(SettingKind {
    String => "string",
    Text => "text",
    Int => "int",
    Bool => "bool",
    Enum => "enum",
});

impl From<weaver_core::config::SettingKind> for SettingKind {
    fn from(kind: weaver_core::config::SettingKind) -> Self {
        match kind {
            weaver_core::config::SettingKind::String => Self::String,
            weaver_core::config::SettingKind::Text => Self::Text,
            weaver_core::config::SettingKind::Int => Self::Int,
            weaver_core::config::SettingKind::Bool => Self::Bool,
            weaver_core::config::SettingKind::Enum => Self::Enum,
        }
    }
}

wire_enum!(SettingSource {
    Default => "default",
    Deployment => "deployment",
    Runtime => "runtime",
});

impl From<weaver_core::config::SettingSource> for SettingSource {
    fn from(source: weaver_core::config::SettingSource) -> Self {
        match source {
            weaver_core::config::SettingSource::Default => Self::Default,
            weaver_core::config::SettingSource::Deployment => Self::Deployment,
            weaver_core::config::SettingSource::Runtime => Self::Runtime,
        }
    }
}

/// One registered setting with all registry metadata and its effective value,
/// as both `settings.get` and `settings.patch` return it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingView {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub default: String,
    pub group: String,
    pub options: Vec<String>,
    pub value: String,
    pub source: SettingSource,
    pub deployment_value: Option<String>,
    pub is_default: bool,
}

impl From<weaver_core::config::SettingView> for SettingView {
    fn from(setting: weaver_core::config::SettingView) -> Self {
        Self {
            key: setting.spec.key.to_string(),
            label: setting.spec.label.to_string(),
            description: setting.spec.description.to_string(),
            kind: setting.spec.kind.into(),
            default: setting.spec.default.to_string(),
            group: setting.spec.group.to_string(),
            options: setting
                .spec
                .options
                .iter()
                .map(|option| (*option).to_string())
                .collect(),
            value: setting.value,
            source: setting.source.into(),
            deployment_value: setting.deployment_value,
            is_default: setting.is_default,
        }
    }
}

/// The envelope both `settings.get` and `settings.patch` return.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingsEnvelope {
    pub settings: Vec<SettingView>,
}

/// One personal preference with its deployment-wide inherited value.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UserPreferenceView {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub options: Vec<String>,
    pub value: String,
    pub inherited_value: String,
    pub is_overridden: bool,
}

/// Effective personal preferences returned by `preferences.get` and
/// `preferences.patch`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UserPreferencesEnvelope {
    pub preferences: Vec<UserPreferenceView>,
}

/// One variable in the default profile's environment, as the
/// `settings.env.*` compatibility facade returns it. Unlike a profile's own
/// environment metadata ([`ProfileEnvView`]), the value is not redacted —
/// this facade predates the write-only convention profiles use.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentEnvVarView {
    pub name: String,
    pub value: String,
    pub updated_at: String,
}

/// Result of `DELETE /api/watches/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct WatchDeleteResult {
    pub deleted: bool,
    pub id: String,
}

/// Result of firing a watch round on demand (`POST /api/watches/{id}/run`):
/// the round's id and its closed outcome, re-read from the run history once
/// the round finishes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct WatchRunResult {
    pub run_id: i64,
    /// `ok|noop|skipped|error`, or empty if the round row could not be
    /// re-read.
    pub outcome: String,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Managed repositories
//
// Mirrors of `loom-forge::repo`'s `ManagedRepo`/`RecentRepo` and the
// `crates/loom/src/web/repos.rs` / `repo_env.rs` ad hoc response shapes.
// weaver-api depends only on weaver-core, not on loom-forge or loom-store, so
// these types are defined here rather than imported.
// ---------------------------------------------------------------------------

/// A repo registered in the managed store (the slug → (remote, path) mapping
/// that doubles as the clone allowlist). Mirrors `loom_forge::repo::ManagedRepo`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoView {
    /// Canonical GitHub `owner/name`.
    pub slug: String,
    /// The clone source URL.
    pub remote_url: String,
    /// The managed on-disk clone path.
    pub path: String,
    pub created_at: String,
}

/// One recently-used repository. Mirrors `loom_forge::repo::RecentRepo`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecentRepoView {
    pub repo_root: String,
    pub last_used_at: String,
    /// How many tracked branches exist in this repo (may be zero).
    pub active_branches: i64,
}

/// One local git branch of a repo checkout, as `GET /api/repos/branches`
/// reports it — name, its worktree if one is checked out, and whether it is
/// the checkout's current branch.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoBranchView {
    pub name: String,
    pub worktree: Option<String>,
    pub current: bool,
}

/// Result of validating a launch fork point against a repo checkout
/// (`GET /api/repos/revisions/validate`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoRevisionValidationView {
    pub valid: bool,
    pub repo_root: String,
    /// Why resolution failed, when `valid` is false.
    pub message: Option<String>,
}

/// One per-repo environment variable's metadata. Mirrors
/// `loom_store::repo_env::RepoEnvVar`. Values are write-only and never
/// returned.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoEnvVarView {
    pub name: String,
    pub updated_at: String,
}

/// The per-repo environment variables' metadata, as every repo-env mutation
/// returns it so the caller can refresh in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoEnvView {
    pub repo_root: String,
    pub env: Vec<RepoEnvVarView>,
}

/// Result of `shell.restart`: the operator shell's process was replaced, so its
/// working directory and environment are whatever a fresh login gets.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ShellRestartResult {
    pub restarted: bool,
}

// ---------------------------------------------------------------------------
// Agents
//
// Mirrors of `loom_agent::agent`'s `AgentMetadata`/`AgentChoice` and
// `loom_agent::custom_agents::CustomAgent`, plus the ad hoc envelopes
// `GET /api/agents` and the `/api/agents/custom*` mutations return.
// ---------------------------------------------------------------------------

/// One selectable value for an agent's `model` or `effort` choice.  Mirrors
/// `loom_agent::agent::AgentChoice`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentChoiceView {
    pub id: String,
    pub label: String,
}

/// One agent runtime the picker offers — a builtin (`claude`, `codex`) or an
/// operator-defined custom agent. Mirrors `loom_agent::agent::AgentMetadata`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentMetadataView {
    pub kind: String,
    pub label: String,
    pub models: Vec<AgentChoiceView>,
    pub efforts: Vec<AgentChoiceView>,
    pub accepts_raw_model: bool,
    pub supports_hooks: bool,
    /// True for the code-shipped `claude`/`codex`; false for an
    /// operator-defined custom agent (which the UI may edit or delete).
    pub builtin: bool,
    /// Whether this runtime can be driven through ACP.
    pub supports_acp: bool,
    /// The agent's declared execution backend: `"terminal"` or `"acp"`.
    pub protocol: String,
}

/// One operator-defined custom agent definition — a row of the
/// `custom_agents` table and the shape the API returns for the editor.
/// Mirrors `loom_agent::custom_agents::CustomAgent`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CustomAgentView {
    /// The id referenced by the agent list and a session's `agent_kind`.
    pub name: String,
    /// The display name shown in the agent picker.
    pub label: String,
    /// Shell run in the worktree before launch.
    pub setup: String,
    /// The fresh-session launch command; the goal is appended as an argument.
    pub launch: String,
    /// The adopt/resume command (no goal). Blank reuses `launch`.
    pub resume: String,
    /// Whether the agent fires loom's lifecycle hooks.
    pub reports_status: bool,
    /// Execution backend: `"terminal"` or `"acp"`. Blank reads as `"terminal"`.
    pub protocol: String,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /api/agents` — the picker list (builtins + custom) plus the full
/// custom-agent definitions the editor round-trips.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsView {
    pub agents: Vec<AgentMetadataView>,
    pub custom: Vec<CustomAgentView>,
    pub default_agent: String,
}

/// Returned by every `/api/agents/custom*` mutation so the caller can refresh
/// the editor's list in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CustomAgentsView {
    pub custom: Vec<CustomAgentView>,
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

wire_enum!(UserRole {
    Admin => "admin",
    User => "user",
});

/// Which sign-in methods the server currently offers — what the login screen
/// renders. `password` is always available (any user can be given one);
/// `github` is true only once an OAuth app is configured.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuthMethods {
    pub password: bool,
    pub github: bool,
}

/// `GET /api/auth/me` — who the caller is and what the login screen needs. The
/// SPA hits this on load: `authenticated: false` means show the login view.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeView {
    pub authenticated: bool,
    /// The approved username, when authenticated.
    pub username: Option<String>,
    /// The caller's GitHub login, when known.
    pub github_login: Option<String>,
    /// How they authenticated: `loopback` | `token` | `session` | null.
    pub via: Option<String>,
    /// Persisted human role. Scoped automation/session principals have no role.
    pub role: Option<UserRole>,
    /// The sign-in methods on offer (for the login screen).
    pub methods: AuthMethods,
}

/// One API token's non-secret metadata (`GET /api/auth/tokens`). The secret
/// itself is only ever returned once, in [`CreatedTokenView`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TokenView {
    pub id: String,
    pub name: String,
    /// The non-secret leading slice, e.g. `loom_AbCd…`, to tell tokens apart.
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

/// `POST /api/auth/tokens` reply — the one and only time the plaintext token is
/// shown. Store it now; the server keeps only a hash.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreatedTokenView {
    /// The full secret — present once, never retrievable again.
    pub token: String,
    #[serde(flatten)]
    pub info: TokenView,
}

/// Body for `POST /api/auth/login` (username/password).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

/// One approved operator (`GET /api/auth/users`). The password hash is never
/// exposed — only whether one is set.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UserView {
    pub username: String,
    pub github_login: Option<String>,
    pub has_password: bool,
    pub role: UserRole,
    pub created_at: String,
}

/// `GET /api/auth/github/config` — the GitHub App / sign-in setup, secret
/// withheld. loom is driven by a single GitHub App (see [the GitHub
/// trigger](../../../docs/github-trigger.md)): its OAuth client powers
/// "Sign in with GitHub" (`configured`/`client_id`), and the same App's id and
/// private key power the `@loom` trigger (`app_configured`/`app_id`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GithubConfigView {
    /// Whether both a client id and secret are present (sign-in is live).
    pub configured: bool,
    /// The OAuth client id (public). Empty when unset. Read from env-or-settings,
    /// so an env-configured deploy reports the live value, not a blank.
    pub client_id: String,
    /// The callback path to register on the App's OAuth client
    /// (`/api/auth/github/callback`).
    pub callback_path: String,
    /// Whether the App identity (id **and** private key) is configured — i.e.
    /// App-backed `@loom` operations and session GitHub access are available.
    /// Interactive sessions may instead use their launching user's Account PAT.
    /// The same App normally backs sign-in above.
    pub app_configured: bool,
    /// The App's numeric id (public). Empty when unset.
    pub app_id: String,
    /// The App's slug (e.g. `loom-acme`), for its name and a
    /// `github.com/apps/{slug}` link. Empty when unknown (e.g. a hand-configured
    /// App, or one set up before the slug was recorded).
    pub app_slug: String,
}

/// Whether the caller has a personal GitHub token on file, and when it last
/// changed (`GET`/`PUT`/`DELETE /api/auth/github-token`). Write-only: the
/// value itself is never returned, only this status.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GithubTokenStatusView {
    pub set: bool,
    pub updated_at: Option<String>,
}

/// Result of `auth.tokens.revoke`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct RevokeTokenResult {
    pub revoked: bool,
    pub id: String,
}

/// Result of `auth.federations.remove`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct RemoveFederationResult {
    pub removed: bool,
    pub id: String,
}

/// Result of `auth.users.remove`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct RemoveUserResult {
    pub removed: bool,
    pub username: String,
}

// -- Session sub-resources ----------------------------------------------------

/// Result of `sessions.archive`. `kind` is `"session"` for a real session or
/// `"launch_attempt"` when the id named a reservation that never became one
/// (its reserved runtime is torn down and the automation row kept as history).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionArchiveResult {
    pub archived: bool,
    pub kind: String,
    pub branch: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Result of `sessions.mode`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionModeResult {
    pub mode_id: String,
}

/// Result of `sessions.url`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionUrlView {
    pub url: String,
}

/// Result of `sessions.files`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionFilesView {
    pub files: Vec<String>,
}

/// Result of `sessions.raw`: a worktree file's bytes, base64-encoded because
/// JSON cannot carry raw binary, plus the content type guessed from its
/// extension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct SessionRawFileView {
    pub content_type: String,
    pub content_base64: String,
}

/// Result of `sessions.ide_info`: whether the embedded editor is enabled and
/// runnable on this host.
#[derive(
    Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
pub struct SessionIdeInfoView {
    pub enabled: bool,
    pub available: bool,
    pub idle_timeout_secs: i64,
}

/// One journaled ACP chat block, as `sessions.chat` exposes it. `payload` is
/// passed through as JSON; the client renders it by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChatBlockView {
    pub turn: i64,
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

/// The paging cursor `sessions.chat` returns when older blocks remain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ChatCursorView {
    pub turn: i64,
    pub seq: i64,
}

/// Agent-owned controls for the conversation composer, mirrored from the live
/// ACP adapter (or its last persisted snapshot). Kept as ACP-shaped JSON to
/// preserve the extensible protocol surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AcpMetadataView {
    #[serde(default)]
    pub commands: Vec<Value>,
    #[serde(default)]
    pub config_options: Vec<Value>,
    #[serde(default)]
    pub modes: Vec<Value>,
    #[serde(default)]
    pub steering_supported: bool,
}

/// Result of `sessions.chat`: a page of the journal plus the composer state
/// needed to render it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionChatView {
    pub blocks: Vec<ChatBlockView>,
    #[serde(default)]
    pub older_cursor: Option<ChatCursorView>,
    /// The turn currently in flight, if any (ACP only).
    #[serde(default)]
    pub live_turn: Option<i64>,
    /// The permission posture captured when the in-flight turn started; may
    /// differ from a live `current_mode` selection, which applies next turn.
    #[serde(default)]
    pub effective_mode: Option<String>,
    #[serde(default)]
    pub pending_prompt: Option<String>,
    pub metadata: AcpMetadataView,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_mcp_policy_redacts_custom_source_but_keeps_audit_identity() {
        let snapshot = McpPolicySnapshot {
            custom_servers: vec![CustomMcpSnapshot {
                identity: "/ops/status".to_string(),
                group: "ops".to_string(),
                revision: 3,
                digest: "sha256:abc".to_string(),
                server_name: "loom_custom_abc".to_string(),
                tools: vec!["status".to_string()],
                source: "operator-only source".to_string(),
            }],
            ..Default::default()
        };
        let audit = SessionMcpPolicyView::from(&snapshot);
        let encoded = serde_json::to_string(&audit).unwrap();
        assert!(!encoded.contains("operator-only source"));
        assert!(encoded.contains("/ops/status"));
        assert!(encoded.contains("sha256:abc"));
    }
}
