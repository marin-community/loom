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

// ---------------------------------------------------------------------------
// View payloads (responses)
// ---------------------------------------------------------------------------

/// One tag on a branch, as the API exposes it. A `(key, value)` annotation with
/// a reason, author, and timestamp. The well-known keys are `attention` (the
/// agent's self-report) and `triage` (a watch's assessment); any other key
/// is a free-form, quiet pill. Absence is the calm state — there is no `ok` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchView {
    pub id: String,
    /// Short label: the branch name with the optional `weaver/` prefix stripped.
    pub name: String,
    pub title: String,
    pub goal: String,
    /// The agent's current-state message, set via `weaver status`, shown even
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

/// Session-scoped view returned by the `/api/sessions[/...]` endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub status: String,
    pub work_dir: String,
    pub term_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub github_repo: Option<String>,
    /// GitHub issue linked to this session's tracking issue, if any. This is
    /// separate from `branch.github`, which is the pull request created by the
    /// work. The tracking issue remains the source of truth for edits.
    #[serde(default)]
    pub github_issue: Option<GithubIssueRef>,
    pub last_activity_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Branch id of the session that **launched** this one — the parent in the
    /// dashboard's session tree — or `null` for a top-level session.
    pub parent_id: Option<String>,
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
    /// Presentation tier: `"interactive"` fleet work or `"automation"`
    /// machinery. The default `GET /api/sessions` listing hides
    /// automation-class rows; `?automation=true` includes them.
    #[serde(default = "default_class")]
    pub class: String,
    /// Completed agent turns on this session.
    #[serde(default)]
    pub turn_count: i64,
    /// The tracking issue opened (or claimed) for this session's task at launch
    /// — the handle handed back to whoever launched it — or `null` when the
    /// launch tracked nothing. Stored on the session row, so every read path
    /// carries it.
    pub tracking_issue: Option<i64>,
    /// Manual park override for the fleet list's resting shelf: `"parked"` (pinned
    /// to the shelf), `"active"` (kept live even when idle), or `null` (auto — the
    /// client parks it once idle past the threshold). See the `sessions.park`
    /// column.
    pub park: Option<String>,
    /// Manual fleet-list sort key, or `null` to follow the automatic
    /// urgency-then-recency order. Placed rows and untouched rows share one
    /// numeric axis so they interleave.
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
    /// Resolved launch permission posture, immutable for this session.
    #[serde(default)]
    pub launch_mode: String,
    /// Exact, source-redacted MCP capability snapshot stamped at launch.
    #[serde(default)]
    pub mcp_policy: SessionMcpPolicyView,
    pub branch: BranchView,
}

/// Cumulative session cost optionally reported alongside ACP context usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpCost {
    pub amount: f64,
    pub currency: String,
}

/// Current model-context utilization for an ACP session. This is context-window
/// state, not a provider account/rate-limit quota.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpUsage {
    pub used: u64,
    pub size: u64,
    #[serde(default)]
    pub cost: Option<AcpCost>,
}

/// A GitHub issue association carried by a session's tracking issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

fn default_prelude() -> String {
    "weaver".to_string()
}

/// A reusable, named session launch posture. Secret environment values are
/// deliberately excluded; `env` contains names and update timestamps only.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub restricted: bool,
    #[serde(default)]
    pub runtime_permissions: Vec<String>,
    #[serde(default)]
    pub mcp_access: McpAccess,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub env: Vec<ProfileEnvView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpAdapterView {
    pub name: String,
    pub description: String,
    pub server_name: String,
}

/// An inspectable, content-addressed collection of MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCapabilitySetView {
    pub name: String,
    pub group: String,
    pub version: String,
    pub digest: String,
    pub description: String,
    pub adapter: String,
    pub tools: Vec<String>,
}

/// The trusted MCP registry exposed to operators and the settings UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRegistryView {
    pub adapters: Vec<McpAdapterView>,
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<CustomMcpView>,
}

/// Provider-neutral MCP selection carried by a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPolicySnapshot {
    pub selection: McpAccess,
    #[serde(default)]
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<CustomMcpSnapshot>,
}

/// Source-redacted MCP audit policy returned on ordinary session views.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMcpPolicyView {
    pub selection: McpAccess,
    #[serde(default)]
    pub capability_sets: Vec<McpCapabilitySetView>,
    #[serde(default)]
    pub custom_servers: Vec<SessionCustomMcpView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerProcessView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Fully resolved non-secret profile policy without launching a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveProfileView {
    pub profile: ProfileView,
    pub mcp_policy: McpPolicySnapshot,
    pub runtime_permissions: Vec<String>,
    pub mcp_servers: Vec<McpServerProcessView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileProbeView {
    pub ok: bool,
    pub effective: EffectiveProfileView,
    pub errors: Vec<String>,
}

/// Body for creating or replacing a profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileReq {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub agent_kind: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default = "default_class")]
    pub class: String,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub env_clear: bool,
    #[serde(default)]
    pub ambient_allowlist: Vec<String>,
    #[serde(default)]
    pub idle_archive_secs: Option<i64>,
    #[serde(default)]
    pub max_concurrent: i64,
    #[serde(default)]
    pub turn_budget: Option<i64>,
    #[serde(default = "default_prelude")]
    pub prelude: String,
    #[serde(default)]
    pub restricted: bool,
    /// Provider-specific fallback permissions. New integrations should use
    /// `mcp_access`; `allowed_tools` remains a read-compatible input alias.
    #[serde(default, alias = "allowed_tools")]
    pub runtime_permissions: Vec<String>,
    #[serde(default)]
    pub mcp_access: McpAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutProfileEnvReq {
    /// A write-only literal. Exactly one of `value` and `secret_ref` is required.
    #[serde(default)]
    pub value: Option<String>,
    /// A GCP Secret Manager version resource, resolved only at launch/respawn.
    #[serde(default)]
    pub secret_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationTokenReq {
    pub subject: String,
    pub profiles: Vec<String>,
    pub ttl_secs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationTokenView {
    pub token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederateReq {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// One named profile and its authoritative write-only environment declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentProfileReq {
    pub profile: ProfileReq,
    #[serde(default)]
    pub env: Vec<DeploymentProfileEnvReq>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentProfileEnvReq {
    pub name: String,
    /// Omit both fields to preserve an existing write-only value by name.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

/// Declarative runtime resources rendered by Pulumi and reconciled by name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeploymentReq {
    #[serde(default)]
    pub profiles: Vec<DeploymentProfileReq>,
    #[serde(default)]
    pub federations: Vec<FederationReq>,
    /// Remove previously deployment-managed resources omitted from this request.
    #[serde(default)]
    pub prune: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentView {
    pub profiles: Vec<ProfileView>,
    pub federations: Vec<FederationView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReq {
    pub profile: String,
    /// Caller-selected durable key. Verified GitHub callers may leave this
    /// blank to use repository/run/attempt, or provide a bounded deterministic
    /// key (for example an issue body hash) that Loom namespaces to the verified
    /// identity.
    #[serde(default)]
    pub idempotency_key: String,
    #[serde(default = "actions_source")]
    pub source: String,
    /// Stable conversation route for related automation deliveries. Each
    /// idempotency key remains a distinct run; channel deliveries reuse one
    /// live ACP session.
    #[serde(default)]
    pub channel: Option<String>,
    pub session: CreateReq,
}

/// One invocation of Loom's fixed, session-scoped GitHub tool surface.
/// `arguments` is validated against the named tool by the server; the generic
/// envelope keeps MCP transport details out of the REST contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestrictedGithubToolReq {
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestrictedGithubToolView {
    pub text: String,
}

fn actions_source() -> String {
    "actions".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunView {
    pub id: String,
    pub actor_subject: String,
    pub source: String,
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

/// One migration stream's observed and expected state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationStreamView {
    pub stream: String,
    pub current: i64,
    pub expected: i64,
    pub applied: i64,
    pub ready: bool,
}

/// Public readiness response. Liveness remains the process-only `/api/health`;
/// this shape proves that the database and both migration streams are usable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticSessionCount {
    pub status: String,
    pub class: String,
    pub profile: String,
    pub protocol: String,
    pub runner_pool: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticProfileCapacity {
    pub profile: String,
    pub revision: i64,
    pub active: i64,
    /// `None` means unlimited (`max_concurrent = 0`).
    pub maximum: Option<i64>,
    pub available: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticRunCount {
    pub status: String,
    pub source: String,
    pub service_tag: String,
    pub profile: String,
    pub count: i64,
}

/// A redacted recent failed run. Deliberately excludes actor, idempotency key,
/// session id, request body, and raw failure summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticRunFailure {
    pub source: String,
    pub profile: String,
    pub outcome: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticRunSummary {
    pub counts: Vec<DiagnosticRunCount>,
    pub stale_creating: i64,
    pub recent_failures: Vec<DiagnosticRunFailure>,
}

/// Aggregated orphan/error inventory. No session, branch, path, principal, or
/// error text crosses this diagnostics boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticFederation {
    pub name: String,
    pub provider: String,
    pub audience: String,
    pub service_tag: String,
    pub profiles: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Admin-only operational snapshot returned by `/api/diagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// The minimal live snapshot of a GitHub thread `weaver issue show` renders
/// beside the weaver ledger: enough to notice "this was closed / re-titled
/// while I worked". An agent that needs the discussion reads it with `gh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
// references). The projection backs both the SPA chips and `weaver artifact
// show`. See docs/artifacts.md.
// ---------------------------------------------------------------------------

/// An artifact envelope as the API exposes it: identity, kind, title, scope, and
/// its latest revision number.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub rev: i64,
    /// `agent` | `user` — who wrote this revision.
    pub author: String,
    pub created_at: String,
}

/// The live status of one issue referenced from an artifact — what the renderer
/// stamps into a `#N` chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactRefs {
    /// `{"<issue id>": { id, title, status, claimed_branch }}` for every `#N`
    /// the content references.
    #[serde(default)]
    pub issues: std::collections::BTreeMap<String, IssueRefStatus>,
}

/// The full artifact view returned by the artifact GET/PUT: the envelope, the
/// content of the selected (default latest) revision, the version list for a
/// picker, and the projected reference map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactView {
    pub meta: ArtifactMeta,
    /// Raw content of the selected revision — the dashboard renders and edits it.
    pub content: String,
    /// Every revision, newest first, for the version picker.
    pub versions: Vec<ArtifactVersion>,
    /// References found in the content, resolved against the live ledger.
    pub refs: ArtifactRefs,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorDto {
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
}

/// One reply in a thread, as the API exposes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentDto {
    pub seq: i64,
    /// `agent` | `user`.
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A discussion thread on an artifact span: its anchor, status, and comments
/// (oldest first), as the GET/POST thread endpoints expose it.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThreadBody {
    pub base_rev: i64,
    pub anchor: AnchorDto,
    pub body: String,
}

/// Body for `POST /api/sessions/{id}/artifacts/{name}/threads/{tid}/comments`:
/// append a reply to an existing thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCommentBody {
    pub body: String,
}

/// One watch, as the API exposes it. The JSON-bearing columns
/// (`trigger`, `scope`, `params`) are returned as **parsed** structured JSON so
/// a UI never re-parses strings; `capabilities` is a real array; the rest is the
/// stored definition plus its schedule bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
// Request payloads
// ---------------------------------------------------------------------------

/// One launch-time scratch file: a name plus its base64-encoded bytes. JSON
/// can't carry raw binary, so the UI reads each dropped file as base64.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScratchUpload {
    pub name: String,
    #[serde(default)]
    pub content_base64: String,
}

/// Body for `POST /api/sessions`: launch a new session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateReq {
    /// The repo to fork the session's worktree from, as a local path. Ignored
    /// when `repo` (a managed repo) is set; otherwise required.
    #[serde(default)]
    pub cwd: String,
    /// An optional **managed** repository to launch against: a GitHub `owner/name`
    /// slug or URL. When set, loom resolves it against the registered repo
    /// allowlist, clones it into the managed repo store if absent (else fetches),
    /// and uses that checkout as the repo root — `cwd` is then ignored. Unset (the
    /// default) keeps the historical behaviour: fork from `cwd`'s repo.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub issue: Option<i64>,
    /// A GitHub issue number the caller already holds the content of (the
    /// `@loom` trigger, whose webhook payload carries the thread): recorded on
    /// the tracking issue as its GitHub link, with none of `issue`'s
    /// fetch-and-seed. Ignored when `issue` is set.
    #[serde(default)]
    pub github_issue: Option<i64>,
    /// A pre-existing weaver issue id to claim for this session (fan-out
    /// pickup). Seeds title/goal/description and stamps `claimed_branch`.
    #[serde(default)]
    pub claim_issue: Option<i64>,
    #[serde(default)]
    pub existing_branch: Option<String>,
    /// The branch (id or name) of the agent launching this session, when it is
    /// itself a weaver session delegating work. Recorded as the tracking
    /// issue's `source_branch`. The `loom` CLI fills this from `$WEAVER_BRANCH`;
    /// a human/dashboard launch leaves it unset.
    #[serde(default)]
    pub parent_branch: Option<String>,
    /// Model selector accepted by the selected agent type; blank/absent uses
    /// the agent runtime's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning effort ('low' | 'medium' | 'high' | 'xhigh' | 'max');
    /// blank/absent uses the agent runtime's default.
    #[serde(default)]
    pub effort: Option<String>,
    /// Reference files to drop into the new worktree's `scratch/` directory
    /// before the agent launches. Empty/absent for a plain session.
    #[serde(default)]
    pub scratch: Vec<ScratchUpload>,
    /// Execution-backend override: `"terminal"` forces the PTY fallback for a
    /// builtin; `"acp"` opts in explicitly. Blank/absent uses the agent's
    /// declared default (acp for the builtins). Rejected for agents that don't
    /// support the requested backend.
    #[serde(default)]
    pub protocol: Option<String>,
    /// The ACP launch permission posture (`auto` | `bypassPermissions` |
    /// `acceptEdits` | `default` | `plan`). Blank/absent uses the configured
    /// `agent.mode` (which defaults to `auto`: a background classifier vets each
    /// tool call and escalates only risky actions). Ignored for a terminal launch.
    #[serde(default)]
    pub mode: Option<String>,
    /// Session class override: `"interactive"` or `"automation"` (anything else
    /// is rejected). Blank/absent derives from the launch origin
    /// (watch/actions/ops → automation, else interactive). Automation-class
    /// sessions are hidden from the default fleet listing.
    #[serde(default)]
    pub class: Option<String>,
    /// Named launch profile. Blank/absent selects `default`.
    #[serde(default)]
    pub profile: Option<String>,
}

/// Body for `POST /api/sessions/{id}/handoff`: replace the live ACP runtime
/// while keeping the loom session, worktree, and canonical chat journal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandoffReq {
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
}

/// Body for `PATCH /api/sessions/{id}`. Branch-level fields (goal/title/
/// description) are forwarded to the underlying branch row. The attention *level*
/// is set through the tags endpoints (`PUT/DELETE /sessions/{id}/tags/{key}`),
/// not here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchSessionReq {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    /// The agent's current-state message — the prose shown beside the level.
    #[serde(default)]
    pub description: Option<String>,
    /// Park override: `"parked"` pins the row to the resting shelf, `"active"`
    /// keeps it live even when idle, `"auto"` clears the override back to
    /// idle-driven. Absent leaves it unchanged. (JSON can't distinguish `null`
    /// from absent for `Option<String>`, so the reset value is the string
    /// `"auto"`, mapped to a stored `NULL`.)
    #[serde(default)]
    pub park: Option<String>,
    /// New manual sort key for this row (the drag midpoint). Absent leaves the
    /// order unchanged.
    #[serde(default)]
    pub sort_order: Option<f64>,
}

/// Body for `PUT /api/sessions/{id}/tags/{key}`: set (upsert) a tag. The `key`
/// is the path segment; this carries the rest. For a loud key (`attention` |
/// `triage`) `value` is `attention` | `blocked` — to return to calm, `DELETE`
/// the tag rather than setting an `ok` value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagReq {
    pub value: String,
    /// One-line reason accompanying the tag.
    #[serde(default)]
    pub note: String,
    /// Who is setting it (a watch name or `manual`); the server defaults a
    /// missing author.
    #[serde(default)]
    pub by: Option<String>,
}

/// One desired tag in `PUT /api/sessions/{id}/tags`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagInput {
    pub key: String,
    pub value: String,
    /// One-line reason accompanying the tag.
    #[serde(default)]
    pub note: String,
}

/// One exact `(key, value)` tag to clear in the same atomic replacement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagMatch {
    pub key: String,
    pub value: String,
}

/// Body for `PUT /api/sessions/{id}/tags`: atomically replace the tags authored
/// by `by` with `tags`. `clear` is reserved for exact-match lifecycle marks such
/// as `idle: idle` that the caller is explicitly replacing too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetTagsReq {
    #[serde(default)]
    pub tags: Vec<TagInput>,
    #[serde(default)]
    pub clear: Vec<TagMatch>,
    /// The author whose existing tag set is replaced. Defaults to `manual`.
    #[serde(default)]
    pub by: Option<String>,
}

/// Body for `POST /api/sessions/{id}/send`: type a message into the agent pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Body for `POST /api/agent/oneshot`: run a fresh, env-stripped one-shot
/// headless agent (`claude -p`) with `prompt` on stdin and return its stdout
/// as `{output}` (`null` when the agent is absent or fails — callers degrade
/// gracefully). The judgement primitive watch programs reach through the
/// daemon, which owns the agent command and timeout configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOneshotReq {
    pub prompt: String,
    /// Model tier override (`haiku` | `sonnet` | `opus` | `fable`); empty
    /// inherits the agent's default.
    #[serde(default)]
    pub model: String,
    /// Reasoning effort override; empty inherits.
    #[serde(default)]
    pub effort: String,
}

/// Body for `POST /api/branches/{id}/issues`: create an issue claimed by a
/// branch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateIssueReq {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub github_issue: Option<i64>,
}

/// Body for `PATCH /api/issues/{id}`: every mutable field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchIssueReq {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// "open" or "closed".
    #[serde(default)]
    pub status: Option<String>,
    /// GitHub issue mapping as `owner/name#number`. An empty string clears the
    /// mapping; absent leaves it unchanged.
    #[serde(default)]
    pub github: Option<String>,
    /// The branch currently working the issue. `null` clears the claim; absent
    /// leaves it unchanged. Claims are created through session launch, so this
    /// patch field deliberately supports clearing only.
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub claimed_branch: Option<Option<String>>,
}

/// Preserve the PATCH distinction between an absent field and an explicit
/// JSON `null`: serde's ordinary `Option<Option<T>>` handling maps both to the
/// outer `None`.
fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// Body for `POST /api/repos/issues`: create an unclaimed repo-level backlog
/// item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateRepoIssueReq {
    pub repo_root: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub github_issue: Option<i64>,
    /// The branch that filed this backlog item (provenance) — `None` for a
    /// bare API call with no originating branch. When set and it resolves to
    /// a live branch in this repo, an `issue_added` event is recorded there.
    #[serde(default)]
    pub source_branch: Option<String>,
}

/// Body for `PUT /api/sessions/{id}/artifacts/{name}`: a user edit that appends
/// a new revision (`author: user`). `title`/`kind` update the envelope; omit
/// them to keep the current values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// Body for `PUT /api/branches/{id}/artifacts/{name}`: create-or-append,
/// unlike the session-scoped `PUT` (which requires the artifact to already
/// exist). `author` defaults to `agent` — the CLI's writer; a `user` edit goes
/// through the session-scoped route instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactUpsertReq {
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Write the repo-shared scope (`branch_id IS NULL`) instead of this
    /// branch's own copy.
    #[serde(default)]
    pub repo: bool,
}

/// Body for `POST /api/branches/{id}/status`: set the agent's attention level
/// and current-state message in one call. `level` is `ok` | `attention` |
/// `blocked`; an absent or empty `message` leaves the previous one in place.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchStatusReq {
    pub level: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// Body for `POST /api/branches/{id}/events`: append a raw event row (e.g. an
/// agent hook). The one escape hatch for an event kind with no dedicated
/// mutating route of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateEventReq {
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

/// One registered setting with its effective value, as `GET /api/settings`
/// exposes it (a subset of the server's richer `SettingSpec` — key, value,
/// and whether it's unset-and-defaulted are all a CLI needs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingView {
    pub key: String,
    pub value: String,
    pub is_default: bool,
}

/// The envelope `GET /api/settings` returns: `{ "settings": [...] }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsEnvelope {
    pub settings: Vec<SettingView>,
}

/// Body for `POST /api/watches`. JSON-bearing fields take structured JSON
/// (`trigger`/`scope`/`params`), which the server serializes into the stored
/// text columns. Optional fields fall back to the model's defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateWatchReq {
    pub name: String,
    #[serde(default)]
    pub trigger: Option<Value>,
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<i64>,
    /// Whether the watch fires as soon as it is created. Omitted by API
    /// clients that want the model default (disabled); the loom UI sends `true`
    /// so a watcher picked from the builtin registry is live without a separate
    /// manual enable.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Body for `PATCH /api/watches/{id}`: every mutable field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchWatchReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub trigger: Option<Value>,
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<i64>,
}

/// Body for `POST /api/watches/{id}/run`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunWatchReq {
    #[serde(default)]
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Which sign-in methods the server currently offers — what the login screen
/// renders. `password` is always available (any user can be given one);
/// `github` is true only once an OAuth app is configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethods {
    pub password: bool,
    pub github: bool,
}

/// `GET /api/auth/me` — who the caller is and what the login screen needs. The
/// SPA hits this on load: `authenticated: false` means show the login view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeView {
    pub authenticated: bool,
    /// The approved username, when authenticated.
    pub username: Option<String>,
    /// The caller's GitHub login, when known.
    pub github_login: Option<String>,
    /// How they authenticated: `loopback` | `token` | `session` | null.
    pub via: Option<String>,
    /// The sign-in methods on offer (for the login screen).
    pub methods: AuthMethods,
}

/// One API token's non-secret metadata (`GET /api/auth/tokens`). The secret
/// itself is only ever returned once, in [`CreatedTokenView`].
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedTokenView {
    /// The full secret — present once, never retrievable again.
    pub token: String,
    #[serde(flatten)]
    pub info: TokenView,
}

/// Body for `POST /api/auth/tokens`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateTokenReq {
    pub name: String,
    /// Optional lifetime in days; omitted / non-positive means it never expires.
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

/// Body for `POST /api/auth/login` (username/password).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

/// Body for `POST /api/auth/password` — set/change the caller's own password.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetPasswordReq {
    pub new_password: String,
}

/// One approved operator (`GET /api/auth/users`). The password hash is never
/// exposed — only whether one is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserView {
    pub username: String,
    pub github_login: Option<String>,
    pub has_password: bool,
    pub created_at: String,
}

/// Body for `POST /api/auth/users` — approve a new operator.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddUserReq {
    pub username: String,
    #[serde(default)]
    pub github_login: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

/// `GET /api/auth/github/config` — the GitHub App / sign-in setup, secret
/// withheld. loom is driven by a single GitHub App (see [the GitHub
/// trigger](../../../docs/github-trigger.md)): its OAuth client powers
/// "Sign in with GitHub" (`configured`/`client_id`), and the same App's id and
/// private key power the `@loom` trigger (`app_configured`/`app_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// the `@loom` trigger acts through the App rather than the ambient
    /// `GH_TOKEN`. The same App normally backs sign-in above.
    pub app_configured: bool,
    /// The App's numeric id (public). Empty when unset.
    pub app_id: String,
    /// The App's slug (e.g. `loom-acme`), for its name and a
    /// `github.com/apps/{slug}` link. Empty when unknown (e.g. a hand-configured
    /// App, or one set up before the slug was recorded).
    pub app_slug: String,
}

/// Body for `PUT /api/auth/github/config`. The secret is write-only — send it to
/// set it, omit it to leave the stored one untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetGithubConfigReq {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
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
