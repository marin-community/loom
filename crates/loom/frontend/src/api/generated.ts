// Generated from Loom's OpenAPI document by crates/weaver-api/tests/typescript.rs
// — do not edit. Every type below is derived from the same `OperationSpec` that
// answers `/api/openapi.json`.
//
// Regenerate: UPDATE_TYPES=1 cargo test -p weaver-api --test typescript

// -- Shared types ---------------------------------------------------------

/** Cumulative session cost optionally reported alongside ACP context usage. */
export interface AcpCost {
  amount: number;
  currency: string;
}

/**
 * Agent-owned controls for the conversation composer, mirrored from the live
 * ACP adapter (or its last persisted snapshot). Kept as ACP-shaped JSON to
 * preserve the extensible protocol surface.
 */
export interface AcpMetadataView {
  commands: unknown[];
  config_options: unknown[];
  modes: unknown[];
  steering_supported: boolean;
}

/**
 * Current model-context utilization for an ACP session. This is context-window
 * state, not a provider account/rate-limit quota.
 */
export interface AcpUsage {
  cost: AcpCost | null;
  size: number;
  used: number;
}

/**
 * Who may call an operation.
 *
 * This is the axis that replaces excluding administrative or human-only
 * endpoints from the registry: an operator-only action is `Admin`, not absent.
 */
export type ActorPolicy = 'session_self' | 'user' | 'admin' | 'internal' | 'session_only' | 'anonymous';

/** Result of `sessions.github.labels.add`. */
export interface AddLabelsResult {
  labels: string[];
  number: number;
}

/**
 * One selectable value for an agent's `model` or `effort` choice.  Mirrors
 * `loom_agent::agent::AgentChoice`.
 */
export interface AgentChoiceView {
  id: string;
  label: string;
}

/**
 * One variable in the default profile's environment, as the
 * `settings.env.*` compatibility facade returns it and as
 * `loom_store::agent_env` stores it. Unlike a profile's own environment
 * metadata ([`ProfileEnvView`]), the value is not redacted — this facade
 * predates the write-only convention profiles use.
 */
export interface AgentEnvVarView {
  name: string;
  updated_at: string;
  value: string;
}

/**
 * One agent runtime the picker offers — a builtin (`claude`, `codex`) or an
 * operator-defined custom agent. Mirrors `loom_agent::agent::AgentMetadata`.
 */
export interface AgentMetadataView {
  accepts_raw_model: boolean;
  /**
   * True for the code-shipped `claude`/`codex`; false for an
   * operator-defined custom agent (which the UI may edit or delete).
   */
  builtin: boolean;
  efforts: AgentChoiceView[];
  kind: string;
  label: string;
  models: AgentChoiceView[];
  /** The agent's declared execution backend: `"terminal"` or `"acp"`. */
  protocol: string;
  /** Whether this runtime can be driven through ACP. */
  supports_acp: boolean;
  supports_hooks: boolean;
}

/**
 * `GET /api/agents` — the picker list (builtins + custom) plus the full
 * custom-agent definitions the editor round-trips.
 */
export interface AgentsView {
  agents: AgentMetadataView[];
  custom: CustomAgentView[];
  default_agent: string;
}

/**
 * A thread's anchor: the quoted span plus a little surrounding context for
 * disambiguation.
 */
export interface AnchorDto {
  prefix: string;
  quote: string;
  suffix: string;
}

/** Result of `sessions.permissions.answer`. */
export interface AnswerPermissionResult {
  option_id: string;
  resolved: boolean;
}

/**
 * Response from `artifacts.delete`: confirms the artifact and its complete
 * revision and discussion history were removed.
 */
export interface ArtifactDeleteResult {
  deleted: boolean;
  name: string;
}

/**
 * An artifact envelope as the API exposes it: identity, kind, title, scope, and
 * its latest revision number.
 */
export interface ArtifactMeta {
  /** The branch that owns it, or `null` for a repo-shared artifact. */
  branch_id: string | null;
  created_at: string;
  id: number;
  kind: string;
  name: string;
  /** The latest revision number. */
  rev: number;
  title: string;
  updated_at: string;
}

/**
 * The projected reference map an artifact's content names. Keyed by id-as-string
 * so it round-trips cleanly through JSON object keys. v1 projects issues; the
 * `artifact:`/`session:` reference kinds are reserved for later probes.
 */
export interface ArtifactRefs {
  /**
   * `{"<issue id>": { id, title, status, claimed_branch }}` for every `#N`
   * the content references.
   */
  issues: Record<string, IssueRefStatus>;
}

export interface ArtifactTextAnchorDto {
  block_index?: number | null;
  prefix?: string;
  quote: string;
  suffix?: string;
}

/**
 * One revision of an artifact (metadata only — the version picker lists these;
 * content is fetched per-rev through the artifact GET with `?rev=`).
 */
export interface ArtifactVersion {
  /** `agent` | `user` — who wrote this revision. */
  author: string;
  created_at: string;
  rev: number;
}

/**
 * The full artifact view returned by the artifact GET/PUT: the envelope, the
 * content of the selected (default latest) revision, the version list for a
 * picker, and the projected reference map.
 */
export interface ArtifactView {
  /** Raw content of the selected revision — the dashboard renders and edits it. */
  content: string;
  meta: ArtifactMeta;
  /** References found in the content, resolved against the live ledger. */
  refs: ArtifactRefs;
  /** Every revision, newest first, for the version picker. */
  versions: ArtifactVersion[];
}

/**
 * Which sign-in methods the server currently offers — what the login screen
 * renders. `password` is always available (any user can be given one);
 * `github` is true only once an OAuth app is configured.
 */
export interface AuthMethods {
  github: boolean;
  password: boolean;
}

export interface AutomationTokenView {
  expires_at: number;
  token: string;
}

/**
 * A unit of message content. Tool input is kept as raw JSON so the renderer
 * owns all formatting decisions (a shell command fenced as `sh`, a patch as
 * text, anything else as pretty JSON).
 */
export type Block = {
  kind: 'text';
  text: string;
} | {
  kind: 'thinking';
  text: string;
} | {
  input: unknown;
  kind: 'tool_use';
  name: string;
} | {
  is_error: boolean;
  kind: 'tool_result';
  output: string;
} | {
  kind: 'image';
};

/**
 * Compact branch projection embedded in [`SessionSummaryView`]. It carries
 * only the identity, status, search, and GitHub fields fleet surfaces render;
 * large goal text and detail-only metadata remain on [`BranchView`].
 */
export interface BranchSummaryView {
  branch: string;
  description: string;
  github: GithubStatus | null;
  github_pr: number | null;
  id: string;
  name: string;
  repo_root: string;
  tags: TagView[];
  title: string;
}

/**
 * Branch with denormalized open-issue count, returned by `/api/branches` and
 * embedded under `SessionView::branch`.
 */
export interface BranchView {
  base_branch: string;
  branch: string;
  created_at: string;
  /**
   * The agent's current-state message, set via `loom status`, shown even
   * when the branch is calm. The attention *level* is the `attention` tag.
   */
  description: string;
  /**
   * The branch's latest GitHub pull-request snapshot (link, review decision,
   * check rollup), or `null` when GitHub polling is off, the repo has no
   * remote PR, or `gh` is unavailable. Maintained by the loom poll loop.
   */
  github: GithubStatus | null;
  /**
   * A user-selected PR number. `null` means loom discovers the branch's
   * current open PR automatically; this is deliberately separate from the
   * cached `github` snapshot above.
   */
  github_pr: number | null;
  goal: string;
  id: string;
  /** Short label: the branch name with the optional `weaver/` prefix stripped. */
  name: string;
  open_issue_count: number;
  repo_root: string;
  /**
   * Every tag on the branch (the agent's `attention`, a watch's
   * `triage`, and any free-form key), ordered by key. Empty when the branch is
   * calm and unmarked — absence is the default state, there is no `ok` tag.
   */
  tags: TagView[];
  title: string;
  /**
   * Ownership of the unqualified task label: `derived`, `generated`,
   * `user`, or `issue`.
   */
  title_provenance: string;
  updated_at: string;
}

export interface ChangeAnchorDto {
  context_after?: string[];
  context_before?: string[];
  end_line: number;
  hunk_header: string;
  path: ChangePathDto;
  selected: string[];
  side: ChangeSideDto;
  start_line: number;
}

export type ChangeBaseDto = {
  oid: string;
  reference: string;
  state: 'available';
} | {
  reason: ChangeBaseUnavailableReasonDto;
  reference: string;
  state: 'unavailable';
};

export type ChangeBaseUnavailableReasonDto = 'unborn_head' | 'missing_base' | 'no_merge_base';

export type ChangeContentDto = 'text' | 'binary' | 'oversize' | 'unsupported';

export interface ChangeFileDto {
  additions: number | null;
  content: ChangeContentDto;
  deletions: number | null;
  hunks: ChangeHunkDto[];
  old_path: ChangePathDto | null;
  path: ChangePathDto;
  sources: ChangeSourceDto[];
  status: ChangeFileStatusDto;
  truncated: boolean;
}

export type ChangeFileStatusDto = 'added' | 'modified' | 'deleted' | 'renamed' | 'copied' | 'type_changed' | 'untracked';

export interface ChangeHunkDto {
  header: string;
  lines: ChangeLineDto[];
  truncated: boolean;
}

export interface ChangeLimitsDto {
  max_files: number;
  max_hunks_per_file: number;
  max_line_bytes: number;
  max_lines_per_file: number;
  max_total_lines: number;
}

export interface ChangeLineDto {
  kind: ChangeLineKindDto;
  new_line: number | null;
  old_line: number | null;
  text: string;
}

export type ChangeLineKindDto = 'context' | 'addition' | 'deletion';

export interface ChangePathDto {
  /** URL-safe base64 of the exact repo-relative Git path bytes. */
  bytes: string;
  /** Escaped, control-free display form; never used as identity. */
  display: string;
}

export interface ChangeSetDto {
  base: ChangeBaseDto;
  files: ChangeFileDto[];
  head_oid: string | null;
  limits: ChangeLimitsDto;
  totals: ChangeTotalsDto;
  truncated: boolean;
  version: string | null;
}

export type ChangeSideDto = 'old' | 'new';

export type ChangeSourceDto = 'committed' | 'staged' | 'unstaged' | 'untracked';

export interface ChangeTotalsDto {
  additions: number;
  deletions: number;
  files: number;
  truncated: boolean;
}

/** Result of archiving a custom channel. */
export interface ChannelArchiveResult {
  archived: boolean;
}

/**
 * One server-owned destination bound to a durable channel. Agents address the
 * Loom channel; the daemon owns provider coordinates and reports delivery per
 * binding without exposing credentials.
 */
export interface ChannelBindingView {
  id: string;
  kind: string;
  label: string;
  target_session_id: string | null;
}

/** Attempt and outcome for delivery of one channel message to one binding. */
export interface ChannelDeliveryView {
  attempts: number;
  /**
   * Stable identity within the channel, for example `session:<id>` or
   * `slack:origin`.
   */
  binding_id: string;
  /** `session`, `slack_thread`, or a future transport kind. */
  binding_kind: string;
  external_id: string | null;
  last_error: string | null;
  state: string;
  target_session_id: string | null;
  updated_at: string;
}

/** One append-only item in a channel's monotonically sequenced history. */
export interface ChannelMessageView {
  author_id: string;
  author_kind: string;
  body: string;
  channel_id: string;
  created_at: string;
  deliveries: ChannelDeliveryView[];
  id: string;
  kind: string;
  payload: unknown;
  reply_to: string | null;
  seq: number;
  urgency: string;
}

/** The authenticated caller's subscription to a channel. */
export interface ChannelSubscriptionView {
  channel_id: string;
  created_at: string;
  mode: string;
  read_seq: number;
  subject_id: string;
  subject_kind: string;
  updated_at: string;
}

/**
 * One durable communication context. A session channel uses its owning
 * session id as `id`; custom channels have an independent id.
 */
export interface ChannelView {
  archived_at: string | null;
  /**
   * This channel's server-owned delivery bindings. The old MCP `get`/`list`
   * tools fetched these with a second call and merged them in by hand;
   * they are part of the response itself now, so REST, the CLI, and MCP
   * all see the same shape.
   */
  bindings: ChannelBindingView[];
  branch_id: string | null;
  created_at: string;
  created_by: string;
  created_by_kind: string;
  id: string;
  kind: string;
  last_message: ChannelMessageView | null;
  name: string;
  repo_root: string;
  session_id: string | null;
  state: string;
  topic: string;
  unread_count: number;
  unread_urgent_count: number;
}

/**
 * One journaled ACP chat block, as `sessions.chat` exposes it and as
 * `loom_store::chat` journals it. Addressed by `(turn, seq)`. `payload` is
 * passed through as JSON; the client renders it by `kind`.
 */
export interface ChatBlockView {
  created_at: string;
  kind: string;
  payload: unknown;
  seq: number;
  turn: number;
}

/** The paging cursor `sessions.chat` returns when older blocks remain. */
export interface ChatCursorView {
  seq: number;
  turn: number;
}

/**
 * Atomic environment composition for a cloned profile. Inherited values are
 * copied server-side; literal values and secret references are write-only.
 */
export interface CloneProfileEnvironmentReq {
  inherit?: boolean;
  remove?: string[];
  set?: ProfileEnvMutationReq[];
}

/** One reply in a thread, as the API exposes it. */
export interface CommentDto {
  /** `agent` | `user`. */
  author: string;
  body: string;
  created_at: string;
  seq: number;
}

/**
 * Where a comment attaches: a fresh anchored thread, or a reply to one
 * already open.
 */
export type CommentTarget = {
  anchor: AnchorDto;
  /** The artifact revision the anchor was taken from. */
  base_rev: number;
  kind: 'new';
} | {
  kind: 'reply';
  thread_id: number;
};

/** Result of `sessions.config.set`. */
export interface ConfigOptionResult {
  config_id: string;
  metadata: AcpMetadataView;
  value: unknown;
}

/**
 * `POST /api/auth/tokens` reply — the one and only time the plaintext token is
 * shown. Store it now; the server keeps only a hash.
 */
export interface CreatedTokenView {
  created_at: string;
  expires_at: string | null;
  id: string;
  last_used_at: string | null;
  name: string;
  /** The non-secret leading slice, e.g. `loom_AbCd…`, to tell tokens apart. */
  prefix: string;
  /** The full secret — present once, never retrievable again. */
  token: string;
}

/**
 * One operator-defined custom agent definition — a row of the
 * `custom_agents` table and the shape the API returns for the editor.
 * Mirrors `loom_agent::custom_agents::CustomAgent`.
 */
export interface CustomAgentView {
  created_at: string;
  /** The display name shown in the agent picker. */
  label: string;
  /** The fresh-session launch command; the goal is appended as an argument. */
  launch: string;
  /** The id referenced by the agent list and a session's `agent_kind`. */
  name: string;
  /** Execution backend: `"terminal"` or `"acp"`. Blank reads as `"terminal"`. */
  protocol: string;
  /** Whether the agent fires loom's lifecycle hooks. */
  reports_status: boolean;
  /** The adopt/resume command (no goal). Blank reuses `launch`. */
  resume: string;
  /** Shell run in the worktree before launch. */
  setup: string;
  updated_at: string;
}

/**
 * Returned by every `/api/agents/custom*` mutation so the caller can refresh
 * the editor's list in one round trip.
 */
export interface CustomAgentsView {
  custom: CustomAgentView[];
}

/** Response from the scalar `mcps.custom.delete` operation. */
export interface CustomMcpDeleteResult {
  deleted: boolean;
  identity: string;
}

/** Exact executable custom MCP revision stamped onto a session. */
export interface CustomMcpSnapshot {
  digest: string;
  group: string;
  identity: string;
  revision: number;
  server_name: string;
  /**
   * Source is part of the immutable recovery snapshot. It is operator-authored
   * code, never a credential, and is not exposed in ordinary session views.
   */
  source: string;
  tools: string[];
}

/** Latest custom MCP definition and validation result. */
export interface CustomMcpView {
  created_at: string;
  description: string;
  digest: string;
  enabled: boolean;
  group: string;
  identity: string;
  label: string;
  revision: number;
  source: string;
  test_source: string;
  tools: string[];
  updated_at: string;
  validation_message: string;
  validation_state: string;
}

/**
 * Result of `sessions.delete`. `kind` is `"session"` for a real session or
 * `"launch_attempt"` when the id named a reservation that never became one,
 * mirroring [`super::archive::Op`]'s result.
 */
export interface DeleteResult {
  deleted: boolean;
  kind: string;
  warnings: string[];
}

export interface DeploymentProfileEnvReq {
  name: string;
  secret_ref?: string | null;
  /** Omit both fields to preserve an existing write-only value by name. */
  value?: string | null;
}

/** One named profile and its authoritative write-only environment declaration. */
export interface DeploymentProfileReq {
  env?: DeploymentProfileEnvReq[];
  profile: DeploymentReconcileNestedInput;
}

/** Replace a named session-launch profile's policy. */
export interface DeploymentReconcileNestedInput {
  /** Agent runtime this profile launches (e.g. `claude`, `codex`). */
  agent_kind: string;
  ambient_allowlist?: string[];
  class?: string;
  description?: string;
  /** Blank uses the runtime's own default. */
  effort?: string;
  env_clear?: boolean;
  /**
   * Optimistic-concurrency guard: rejects a stale edit with a 409 and the
   * current profile instead of silently overwriting it.
   */
  expected_revision?: number | null;
  /**
   * Repositories for which Loom may broker a short-lived GitHub App
   * token.
   */
  github_repositories?: string[];
  idle_archive_secs?: number | null;
  /**
   * Organization-owned instructions appended to this profile's opening
   * prompt for every launch origin.
   */
  instructions?: string;
  max_concurrent?: number;
  /** Provider-neutral MCP selection: `none`, `all`, or `groups`. */
  mcp_access?: McpAccess;
  /** Blank uses the runtime's own default. */
  mode?: string;
  /** Blank uses the runtime's own default. */
  model?: string;
  /** The profile's name. */
  name: string;
  prelude?: string;
  /** Blank uses the runtime's own default. */
  protocol?: string;
  restricted?: boolean;
  /** Provider-specific fallback permissions. */
  runtime_permissions?: string[];
  strict?: boolean;
  turn_budget?: number | null;
}

/** A scalar setting value in a JSON or YAML deployment manifest. */
export type DeploymentSettingValue = string | number | boolean;

export interface DeploymentView {
  federations: FederationView[];
  profiles: ProfileView[];
  settings: SettingView[];
}

/**
 * Non-secret federation mapping metadata useful for verifying deployment
 * reconciliation. This never includes a bearer/OIDC token or signing key.
 */
export interface DiagnosticFederation {
  audience: string;
  created_at: string;
  name: string;
  profiles: string[];
  provider: string;
  service_tag: string;
  updated_at: string;
}

/**
 * Aggregated orphan/error inventory. No session, branch, path, principal, or
 * error text crosses this diagnostics boundary.
 */
export interface DiagnosticProblemSummary {
  class: string;
  count: number;
  latest_activity_at: string | null;
  profile: string;
  protocol: string;
  runner_pool: string;
  status: string;
}

export interface DiagnosticProfileCapacity {
  active: number;
  available: number | null;
  /** `None` means unlimited (`max_concurrent = 0`). */
  maximum: number | null;
  profile: string;
  revision: number;
}

export interface DiagnosticRunCount {
  count: number;
  profile: string;
  service_tag: string;
  source: string;
  status: string;
}

/**
 * A redacted recent failed run. Deliberately excludes actor, idempotency key,
 * session id, request body, and raw failure summary.
 */
export interface DiagnosticRunFailure {
  outcome: string | null;
  profile: string;
  source: string;
  updated_at: string;
}

export interface DiagnosticRunSummary {
  counts: DiagnosticRunCount[];
  recent_failures: DiagnosticRunFailure[];
  stale_creating: number;
}

/**
 * A session count across every bounded control-plane dimension available in
 * the current schema. `runner_pool` is `local` until runner pools land.
 */
export interface DiagnosticSessionCount {
  class: string;
  count: number;
  profile: string;
  protocol: string;
  runner_pool: string;
  status: string;
}

/** Human-readable operational snapshot returned by `/api/diagnostics`. */
export interface DiagnosticsView {
  automation_runs: DiagnosticRunSummary;
  federations: DiagnosticFederation[];
  migrations: MigrationStreamView[];
  problems: DiagnosticProblemSummary[];
  profiles: DiagnosticProfileCapacity[];
  sessions: DiagnosticSessionCount[];
}

/** Current Loom operation grants and external repository scope for a session. */
export interface EffectivePermissionsView {
  actor: string;
  github_repositories: string[];
  operations: string[];
  pending_requests: PermissionRequestView[];
  session_id: string;
}

/** Fully resolved non-secret profile policy without launching a session. */
export interface EffectiveProfileView {
  mcp_policy: McpPolicySnapshot;
  mcp_servers: McpServerProcessView[];
  profile: ProfileView;
  runtime_permissions: string[];
}

export interface Event {
  branch_id: string;
  created_at: string;
  data: unknown;
  id: number;
  kind: string;
}

export interface FederationReq {
  audience: string;
  event_name?: string | null;
  issuer?: string;
  /** Stable operator-owned identity used for idempotent reconciliation. */
  name: string;
  profiles: string[];
  provider?: string;
  ref_pattern?: string | null;
  repository_id?: string | null;
  /** Exact verified Google service-account email. */
  service_account?: string | null;
  /** Stable, bounded audit label copied into Loom automation credentials. */
  service_tag: string;
  /** Exact numeric OIDC subject for Google workload identities. */
  subject?: string | null;
  workflow_ref?: string | null;
}

export interface FederationView {
  audience: string;
  created_at: string;
  event_name: string | null;
  id: string;
  issuer: string;
  name: string;
  profiles: string[];
  provider: string;
  ref_pattern: string | null;
  repository_id: string | null;
  service_account: string | null;
  service_tag: string;
  subject: string | null;
  updated_at: string;
  workflow_ref: string | null;
}

/**
 * `GET /api/auth/github/config` — the GitHub App / sign-in setup, secret
 * withheld. loom is driven by a single GitHub App (see [the GitHub
 * trigger](../../../docs/github-trigger.md)): its OAuth client powers
 * "Sign in with GitHub" (`configured`/`client_id`), and the same App's id and
 * private key power the `@loom` trigger (`app_configured`/`app_id`).
 */
export interface GithubConfigView {
  /**
   * Whether the App identity (id **and** private key) is configured — i.e.
   * App-backed `@loom` operations and session GitHub access are available.
   * Interactive sessions may instead use their launching user's Account PAT.
   * The same App normally backs sign-in above.
   */
  app_configured: boolean;
  /** The App's numeric id (public). Empty when unset. */
  app_id: string;
  /**
   * The App's slug (e.g. `loom-acme`), for its name and a
   * `github.com/apps/{slug}` link. Empty when unknown (e.g. a hand-configured
   * App, or one set up before the slug was recorded).
   */
  app_slug: string;
  /**
   * The callback path to register on the App's OAuth client
   * (`/api/auth/github/callback`).
   */
  callback_path: string;
  /**
   * The OAuth client id (public). Empty when unset. Read from env-or-settings,
   * so an env-configured deploy reports the live value, not a blank.
   */
  client_id: string;
  /** Whether both a client id and secret are present (sign-in is live). */
  configured: boolean;
}

/** A GitHub issue association carried by a session's explicit work item. */
export interface GithubIssueRef {
  number: number;
  repo: string;
}

/**
 * A branch's pull-request snapshot, as stored and as served under
 * `BranchView::github`. `pr_state` is `OPEN` / `CLOSED` / `MERGED`; `checks` is
 * the rolled-up `passing` / `failing` / `pending` (or `null` when the PR has no
 * checks); `review_decision` is GitHub's `APPROVED` / `CHANGES_REQUESTED` /
 * `REVIEW_REQUIRED` (or `null` when review isn't required). `head_updated_at`
 * is the time associated with the current `head_sha`; the poller preserves it
 * while the head is unchanged so unrelated PR metadata does not make code look
 * newly pushed.
 */
export interface GithubStatus {
  checks: string | null;
  fetched_at: string;
  head_sha: string | null;
  head_updated_at: string | null;
  is_draft: boolean;
  mergeable: string | null;
  merged_at: string | null;
  pr_number: number;
  pr_state: string;
  pr_title: string;
  pr_url: string;
  review_decision: string | null;
}

/**
 * The minimal live snapshot of a GitHub thread `loom issues get` renders
 * beside the weaver ledger: enough to notice "this was closed / re-titled
 * while I worked". An agent that needs the discussion reads it with `gh`.
 */
export interface GithubThreadState {
  /** `open` | `closed`. */
  state: string;
  title: string;
  /** ISO time of the thread's last touch, as GitHub reports it. */
  updated_at: string;
}

/**
 * Whether the caller has a personal GitHub token on file, and when it last
 * changed (`GET`/`PUT`/`DELETE /api/auth/github-token`). Write-only: the
 * value itself is never returned, only this status.
 */
export interface GithubTokenStatusView {
  set: boolean;
  updated_at: string | null;
}

/** A short-lived GitHub App installation token brokered for one session. */
export interface GithubTokenView {
  token: string;
}

/** The kind of one normalized history record. */
export type HistoryKind = 'message' | 'reasoning' | 'tool_call' | 'tool_result' | 'context' | 'event' | 'image';

/** A source-provided file location attached to a normalized history record. */
export interface HistoryLocationView {
  line: number | null;
  path: string;
}

/**
 * One newest-tail page of normalized session history. Records are returned in
 * chronological display order; pass `older_cursor` as `before` to continue
 * backward. The same envelope is used by literal search.
 */
export interface HistoryPageView {
  older_cursor: string | null;
  records: HistoryRecordView[];
  /** Normalizer/source label (`acp`, `claude`, `codex`, ...). */
  source: string;
}

/**
 * One provider-neutral conversation record returned by the session history
 * API. Optional fields are capability claims, not placeholders: notably,
 * `tool_input` is absent when the source protocol did not provide invocation
 * arguments (ACP currently provides only tool title/status/content/locations).
 */
export interface HistoryRecordView {
  content: string | null;
  /** Opaque, source-stable position used as the exclusive paging cursor. */
  cursor: string;
  event_name: string | null;
  is_error: boolean | null;
  /**
   * `message`, `reasoning`, `tool_call`, `tool_result`, `context`, `event`,
   * or `image`.
   */
  kind: string;
  locations: HistoryLocationView[];
  role: string | null;
  timestamp: string | null;
  tool_input: unknown;
  tool_name: string | null;
  tool_status: string | null;
}

/**
 * How an operation's response is encoded.
 *
 * This is the *only* axis on which a registered operation may be special.
 * Streaming and upload endpoints keep their descriptor, typed input, and
 * authorization instead of becoming unchecked special cases.
 */
export type Io = 'json' | 'stream' | 'duplex' | 'upload' | 'download' | 'session';

/** One command validated and applied atomically to every requested issue. */
export type IssueAction = {
  type: 'close';
} | {
  type: 'reopen';
} | {
  by?: string | null;
  key: string;
  note?: string;
  type: 'tag';
  value: string;
} | {
  key: string;
  type: 'untag';
} | {
  type: 'delete';
};

/** Aggregate outcome from a successful atomic `POST /api/issues/actions`. */
export interface IssueActionsResult {
  /** Deleted IDs for delete. Empty for every other action. */
  deleted_ids: number[];
  /** Updated issue views for close, reopen, tag, and untag. */
  issues: IssueView[];
}

/**
 * The live status of one issue referenced from an artifact — what the renderer
 * stamps into a `#N` chip.
 */
export interface IssueRefStatus {
  /** The branch working it; `null` is the unclaimed backlog. */
  claimed_branch: string | null;
  id: number;
  /** `open` | `closed`. */
  status: string;
  title: string;
}

/** One initial tag supplied while creating an issue. */
export interface IssueTagInput {
  by?: string | null;
  key: string;
  note?: string;
  value: string;
}

/** Issue as the API exposes it. */
export interface IssueView {
  body: string;
  /** Branch currently working it; `null` is the unclaimed repo backlog. */
  claimed_branch: string | null;
  closed_at: string | null;
  created_at: string;
  github_issue: number | null;
  github_repo: string | null;
  /**
   * Live state of the linked GitHub thread, fetched at read time by the
   * single-issue endpoint when the issue carries a `github_repo` +
   * `github_issue` link. Absent on list endpoints (no fan-out of GitHub
   * calls) and when the fetch fails — the ledger fields above still stand.
   */
  github_state: GithubThreadState | null;
  id: number;
  repo_root: string;
  /** Branch the issue was created from (provenance). */
  source_branch: string | null;
  status: string;
  /**
   * Free-form `(key, value)` labels on the issue, rendered as quiet pills.
   * Empty when the issue carries none. Unlike branch tags these never carry
   * the loud `attention`/`triage` ladder.
   */
  tags: TagView[];
  title: string;
  updated_at: string;
}

/**
 * Capacity observed while resolving a launch. The repository launch gate
 * rechecks it immediately before provisioning, so this is an honest preview,
 * not an admission reservation.
 */
export interface LaunchCapacityView {
  active: number;
  allowed: boolean;
  available: number | null;
  maximum: number | null;
}

/**
 * Fields a caller may layer over a named profile for one launch. Presence is
 * significant: an omitted (or blank agent) field inherits while an explicit
 * empty model or effort selects the agent's own default.
 */
export interface LaunchOverrides {
  agent?: string | null;
  class?: string | null;
  effort?: string | null;
  mode?: string | null;
  model?: string | null;
  protocol?: string | null;
}

/** Provenance for every concrete runtime selector in a resolved launch. */
export interface LaunchProvenanceView {
  agent: string;
  class: string;
  effort: string;
  idle_archive_secs: string;
  mode: string;
  model: string;
  protocol: string;
  turn_budget: string;
}

/**
 * Canonical profile-template selection accepted by launch preview, session
 * create, handoff, and profile clone.
 */
export interface LaunchSelection {
  overrides?: LaunchOverrides;
  profile?: string;
}

/**
 * A whole conversation, normalized. `source` names the agent it came from
 * (`"claude"`, `"codex"`); the rest is optional context the renderer banners.
 */
export interface Log {
  cwd: string | null;
  messages: Message[];
  model: string | null;
  session_id: string | null;
  source: string;
}

/** One captured log line, as the UI renders it. */
export interface LogLineView {
  /** `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`. */
  level: string;
  /** The rendered message plus any structured fields. */
  message: string;
  /**
   * Monotonic sequence number, so the UI can dedupe the snapshot against
   * the live stream (and detect drops) without comparing timestamps.
   */
  seq: number;
  /** The event's target (module path, e.g. `loom::web::repos`). */
  target: string;
  /** RFC3339 UTC timestamp. */
  ts: string;
}

/** Provider-neutral MCP selection carried by a profile. */
export interface McpAccess {
  groups?: string[];
  /** `none`, `all`, or `groups`. */
  mode: string;
}

/**
 * One trusted MCP adapter Loom can launch.  This is deliberately provider
 * neutral: clients select a capability set, while an agent runtime translates
 * its tools into that provider's permission vocabulary.
 */
export interface McpAdapterView {
  description: string;
  name: string;
  server_name: string;
}

/** An inspectable, content-addressed collection of MCP tools. */
export interface McpCapabilitySetView {
  adapter: string;
  /** Canonical replacement for a compatibility-only capability identity. */
  deprecated_by: string | null;
  description: string;
  digest: string;
  group: string;
  name: string;
  tools: string[];
  version: string;
}

/** Exact MCP registry content stamped onto a launched session. */
export interface McpPolicySnapshot {
  capability_sets: McpCapabilitySetView[];
  custom_servers: CustomMcpSnapshot[];
  selection: McpAccess;
}

/** The trusted MCP registry exposed to operators and the settings UI. */
export interface McpRegistryView {
  adapters: McpAdapterView[];
  capability_sets: McpCapabilitySetView[];
  custom_servers: CustomMcpView[];
}

export interface McpServerProcessView {
  args: string[];
  command: string;
  name: string;
}

/**
 * `GET /api/auth/me` — who the caller is and what the login screen needs. The
 * SPA hits this on load: `authenticated: false` means show the login view.
 */
export interface MeView {
  authenticated: boolean;
  /** The caller's GitHub login, when known. */
  github_login: string | null;
  /** The sign-in methods on offer (for the login screen). */
  methods: AuthMethods;
  /** Persisted human role. Scoped automation/session principals have no role. */
  role: UserRole | null;
  /** The approved username, when authenticated. */
  username: string | null;
  /** How they authenticated: `loopback` | `token` | `session` | null. */
  via: string | null;
}

/** One message: who said it, when, and its ordered content blocks. */
export interface Message {
  blocks: Block[];
  role: Role;
  timestamp: string | null;
}

/** One migration stream's observed and expected state. */
export interface MigrationStreamView {
  applied: number;
  current: number;
  expected: number;
  ready: boolean;
  stream: string;
}

export type OperationRisk = 'read' | 'write' | 'destructive' | 'external_write';

/** The durable resource an operation is authorized against. */
export type OperationScope = 'session' | 'branch' | 'repository' | 'global';

export interface OperationView {
  actor: ActorPolicy;
  bundle: string;
  cli: string | null;
  cli_aliases: string[];
  grants: string[];
  id: string;
  io: Io;
  method: string;
  output_schema: unknown;
  path: string;
  risk: OperationRisk;
  schema: unknown;
  scope: OperationScope;
  summary: string;
}

/** Durable request for a human to expand one live session's external access. */
export interface PermissionRequestView {
  decided_at: string | null;
  decided_by: string | null;
  decision_reason: string | null;
  id: string;
  kind: string;
  mode: string;
  reason: string;
  repository: string;
  requested_at: string;
  requested_by: string;
  session_id: string;
  state: string;
}

/** Response from the scalar `profiles.delete` operation. */
export interface ProfileDeleteResult {
  deleted: boolean;
  name: string;
}

export interface ProfileEnvMutationReq {
  name: string;
  secret_ref?: string | null;
  value?: string | null;
}

export interface ProfileEnvView {
  name: string;
  secret_ref: string | null;
  /** `literal` or `gcp_secret`. The value itself is never returned. */
  source: string;
  updated_at: string;
}

/**
 * A reusable, named session launch template. It is concretized into an
 * immutable `ResolvedLaunchView` for each accepted launch/handoff. Secret
 * environment values are excluded; `env` contains metadata only.
 */
export interface ProfileView {
  agent_kind: string;
  ambient_allowlist: string[];
  class: string;
  created_at: string;
  description: string;
  effort: string;
  env: ProfileEnvView[];
  env_clear: boolean;
  github_repositories: string[];
  idle_archive_secs: number | null;
  /**
   * Organization-owned instructions appended to this profile's opening
   * prompt for every launch origin.
   */
  instructions: string;
  /**
   * Servers predating profile lifetimes expose only the original selectable
   * lifetime, so a newer typed client can safely interpret omission as 1.
   */
  lifetime: number;
  max_concurrent: number;
  mcp_access: McpAccess;
  mode: string;
  model: string;
  name: string;
  prelude: string;
  protocol: string;
  restricted: boolean;
  revision: number;
  runtime_permissions: string[];
  strict: boolean;
  turn_budget: number | null;
  updated_at: string;
}

/** Replace a named session-launch profile's policy. */
export interface ProfilesCloneNestedInput {
  /** Agent runtime this profile launches (e.g. `claude`, `codex`). */
  agent_kind: string;
  ambient_allowlist?: string[];
  class?: string;
  description?: string;
  /** Blank uses the runtime's own default. */
  effort?: string;
  env_clear?: boolean;
  /**
   * Optimistic-concurrency guard: rejects a stale edit with a 409 and the
   * current profile instead of silently overwriting it.
   */
  expected_revision?: number | null;
  /**
   * Repositories for which Loom may broker a short-lived GitHub App
   * token.
   */
  github_repositories?: string[];
  idle_archive_secs?: number | null;
  /**
   * Organization-owned instructions appended to this profile's opening
   * prompt for every launch origin.
   */
  instructions?: string;
  max_concurrent?: number;
  /** Provider-neutral MCP selection: `none`, `all`, or `groups`. */
  mcp_access?: McpAccess;
  /** Blank uses the runtime's own default. */
  mode?: string;
  /** Blank uses the runtime's own default. */
  model?: string;
  /** The profile's name. */
  name: string;
  prelude?: string;
  /** Blank uses the runtime's own default. */
  protocol?: string;
  restricted?: boolean;
  /** Provider-specific fallback permissions. */
  runtime_permissions?: string[];
  strict?: boolean;
  turn_budget?: number | null;
}

/**
 * One **program** a watch can run, as `GET /api/watches/programs`
 * exposes it. Builtin programs are Python scripts that ship inside the loom
 * binary; the embedded source is returned for a read-only view in the panel.
 */
export interface ProgramView {
  /**
   * Suggested starting config for a new watch running this program:
   * `{trigger, scope, params, capabilities}` — what a create form prefills.
   */
  defaults: unknown;
  description: string;
  /**
   * The reference a watch's `program` field names it by, e.g.
   * `builtin:status` or `builtin:archive-merged`.
   */
  program: string;
  /**
   * The program's embedded Python source. Read-only — it ships with the
   * binary.
   */
  source: string;
  title: string;
}

/**
 * Result of `sessions.prompt.create`. Mirrors the ACP task's own
 * acknowledgement (`queued`, `turn`), the same shape `sessions.send` returns
 * for an ACP session.
 */
export interface PromptResult {
  queued: boolean;
  turn: number | null;
}

/** One recently-used repository. Mirrors `loom_forge::repo::RecentRepo`. */
export interface RecentRepoView {
  /** How many tracked branches exist in this repo (may be zero). */
  active_branches: number;
  last_used_at: string;
  repo_root: string;
}

/** Result of `auth.federations.remove`. */
export interface RemoveFederationResult {
  id: string;
  removed: boolean;
}

/** Result of `auth.users.remove`. */
export interface RemoveUserResult {
  removed: boolean;
  username: string;
}

/**
 * One local git branch of a repo checkout, as `GET /api/repos/branches`
 * reports it — name, its worktree if one is checked out, and whether it is
 * the checkout's current branch.
 */
export interface RepoBranchView {
  current: boolean;
  name: string;
  worktree: string | null;
}

/**
 * One per-repo environment variable's metadata, and the row type
 * `loom_store::repo_env` reads. The value is deliberately omitted: per-repo
 * variables are write-only, so a stored secret can be replaced but never read
 * back.
 */
export interface RepoEnvVarView {
  name: string;
  updated_at: string;
}

/**
 * The per-repo environment variables' metadata, as every repo-env mutation
 * returns it so the caller can refresh in one round trip.
 */
export interface RepoEnvView {
  env: RepoEnvVarView[];
  repo_root: string;
}

/**
 * Result of validating a launch fork point against a repo checkout
 * (`GET /api/repos/revisions/validate`).
 */
export interface RepoRevisionValidationView {
  /** Why resolution failed, when `valid` is false. */
  message: string | null;
  repo_root: string;
  valid: boolean;
}

/**
 * A repo registered in the managed store (the slug → (remote, path) mapping
 * that doubles as the clone allowlist). Mirrors `loom_forge::repo::ManagedRepo`.
 */
export interface RepoView {
  created_at: string;
  /** The managed on-disk clone path. */
  path: string;
  /** The clone source URL. */
  remote_url: string;
  /** Canonical GitHub `owner/name`. */
  slug: string;
}

/**
 * Source-redacted security and lifecycle policy that will be stamped on the
 * session. Environment values and custom MCP source are deliberately absent.
 */
export interface ResolvedLaunchPolicyView {
  ambient_allowlist: string[];
  env_clear: boolean;
  environment: ProfileEnvView[];
  github_repositories: string[];
  idle_archive_secs: number | null;
  instructions: string;
  mcp_policy: SessionMcpPolicyView;
  prelude: string;
  restricted: boolean;
  runtime_permissions: string[];
  strict: boolean;
  turn_budget: number | null;
}

/**
 * Concrete source-redacted immutable launch snapshot returned by preview and
 * exposed on the created session (or replacement handoff runtime).
 */
export interface ResolvedLaunchView {
  agent: string;
  capacity: LaunchCapacityView;
  class: string;
  effort: string;
  errors: string[];
  locked_fields: string[];
  mode: string;
  model: string;
  policy: ResolvedLaunchPolicyView;
  profile_lifetime: number;
  profile_revision: number;
  protocol: string;
  provenance: LaunchProvenanceView;
  resolver_revision: string;
  selection: LaunchSelection;
  valid: boolean;
}

export interface RestrictedGithubToolView {
  text: string;
}

export interface ResumptionCueView {
  evidence: ResumptionEvidenceView[];
  generated_at: string | null;
  source_cursor: string | null;
  /**
   * `generated`, `generating`, `due`, `not_due`, `disabled`, or
   * `unavailable`.
   */
  status: string;
  text: string | null;
}

export interface ResumptionEvidenceView {
  /** Source-stable history cursor or immutable artifact id/revision cursor. */
  cursor: string;
  href: string;
  /** `conversation` or `artifact`. */
  kind: string;
  label: string;
}

/** Result of `sessions.prompt.retract`: the retracted text. */
export interface RetractResult {
  text: string;
}

export type ReviewAnchorDto = ArtifactTextAnchorDto | ChangeAnchorDto;

export type ReviewAnchorKindDto = 'text' | 'change';

export interface ReviewCommentDto {
  anchor: ReviewAnchorDto;
  anchor_kind: ReviewAnchorKindDto;
  body: string;
  created_at: string;
  id: number;
  status: string;
  subject_version: string;
  updated_at: string;
}

export interface ReviewDto {
  acknowledged_outdated: boolean;
  comments: ReviewCommentDto[];
  created_at: string;
  created_by: string;
  delivery_error: string | null;
  delivery_key: string;
  delivery_state: string;
  /** Monotonic optimistic revision for the editable draft envelope. */
  draft_revision: number;
  id: number;
  legacy: boolean;
  /** Server-authoritative exact conversation payload preview. */
  message: string;
  outdated: boolean;
  session_id: string;
  status: string;
  subject: ReviewSubjectDto;
  submitted_at: string | null;
  summary: string;
  updated_at: string;
}

export interface ReviewSubjectDto {
  current_version: string;
  /** Stable internal artifact envelope id. */
  id: string;
  /** Stable public subject key: the artifact name accepted by list/create. */
  key: string;
  kind: ReviewSubjectKindDto;
  label: string;
  version: string;
}

export type ReviewSubjectKindDto = 'artifact' | 'changes';

/** Result of `auth.tokens.revoke`. */
export interface RevokeTokenResult {
  id: string;
  revoked: boolean;
}

/**
 * Who a [`Message`] is from. `Context` is injected, non-conversational material
 * (a session primer, system/permissions instructions) — kept for completeness
 * but rendered out of the way.
 */
export type Role = 'user' | 'assistant' | 'context';

export interface RunView {
  actor_subject: string;
  channel: string | null;
  created_at: string;
  id: string;
  idempotency_key: string;
  outcome: string | null;
  profile: string;
  service_tag: string;
  session_id: string;
  source: string;
  status: string;
  summary: string;
  updated_at: string;
  watch_id: string | null;
}

/** Launch a child session from a task or claimed work item. */
export interface RunsCreateNestedInput {
  /** Agent runtime to launch; blank uses the profile's default. */
  agent?: string | null;
  /** Base branch or ref to fork from. */
  base?: string | null;
  /** A pre-existing Loom backlog item to claim for this session. */
  claim_issue?: number | null;
  /**
   * Session class override: `"interactive"` or `"automation"` (anything
   * else is rejected). Blank/absent derives from the launch origin
   * (watch/actions/ops/grafana → automation, else interactive).
   */
  class?: string | null;
  /**
   * Local worktree path to fork the session's worktree from, when not
   * launching against a managed `repo`.
   */
  cwd?: string;
  /** Reasoning-effort override. */
  effort?: string | null;
  /** Attach to a branch that already exists rather than creating one. */
  existing_branch?: string | null;
  /**
   * Optimistic-concurrency guards: the profile and resolver revisions the
   * caller previewed against. A launch whose configuration changed underneath
   * it is rejected rather than silently run with different settings.
   */
  expected_profile_revision?: number | null;
  /** The resolver revision is a content hash, not a counter. */
  expected_resolver_revision?: string | null;
  /** A GitHub issue number to link the session to. */
  github_issue?: number | null;
  /** Detailed goal for the new session; defaults to the task label. */
  goal?: string | null;
  /** An existing GitHub issue number to seed the session from. */
  issue?: number | null;
  /**
   * The ACP launch permission posture (`auto` | `bypassPermissions` |
   * `acceptEdits` | `default` | `plan`). Blank/absent uses the configured
   * `agent.mode` (which defaults to `auto`). Ignored for a terminal launch.
   */
  mode?: string | null;
  /** Model override, when the profile's default is not wanted. */
  model?: string | null;
  /** Explicit branch name instead of a generated one. */
  name?: string | null;
  /**
   * The branch of the launching session, when this is an agent-delegated
   * launch. Filled from the caller's own branch; a human/dashboard launch
   * leaves it unset.
   */
  parent_branch?: string | null;
  /** Named launch profile; blank selects `default`. */
  profile?: string | null;
  /**
   * Execution-backend override: `"terminal"` forces the PTY fallback for a
   * builtin; `"acp"` opts in explicitly. Blank/absent uses the agent's
   * declared default (acp for the builtins). Rejected for agents that don't
   * support the requested backend.
   */
  protocol?: string | null;
  /** A managed repository (GitHub `owner/name`) to launch against. */
  repo?: string | null;
  /** Files to seed the session's scratch directory with. */
  scratch?: ScratchUpload[];
  /**
   * The resolved profile and per-launch overrides.
   *
   * Carries the agent, model, effort, and MCP access the caller previewed.
   */
  selection?: LaunchSelection | null;
  /**
   * One-line task label for the new session.
   *
   * Optional: derived from a claimed issue or managed repo branch name if omitted.
   */
  title?: string | null;
}

/** Result of deleting a Scratch file. */
export interface ScratchDeleteResult {
  deleted: boolean;
  name: string;
}

/** One file in a session's Scratch directory. */
export interface ScratchFileView {
  bytes: number;
  name: string;
}

/**
 * Shared upload limits for launch-time and live-session Scratch attachments:
 * 20 files, 25 MiB each, 50 MiB decoded total. `.gitignore` is reserved.
 */
export interface ScratchLimitsView {
  max_file_bytes: number;
  max_files: number;
  max_name_bytes: number;
  max_total_bytes: number;
}

/**
 * One launch-time scratch file: a name plus its base64-encoded bytes. JSON
 * can't carry raw binary, so the UI reads each dropped file as base64.
 */
export interface ScratchUpload {
  content_base64?: string;
  name: string;
}

/**
 * Result of writing a Scratch file: the accepted name, its size, and the
 * worktree-relative path the session sees it at.
 */
export interface ScratchWriteResult {
  bytes: number;
  name: string;
  path: string;
}

/**
 * Where a session reads its own channel, artifacts, and session record.
 *
 * Each value is an operation's path, not a per-id URL: the operand these three
 * reads take is the caller's own context, so a session credential posting an
 * empty body to any of them gets its own.
 */
export interface SelfContextLinks {
  artifacts: string;
  channel: string;
  session: string;
}

/**
 * Caller-relative bootstrap context used by in-session tools. REST resources
 * remain canonically id-addressed; this view resolves `self` once.
 */
export interface SelfContextView {
  branch_id: string;
  /**
   * The branch's human name. Carried alongside the id because context fields
   * need both and confusing them is silent — see `ContextSource::BranchName`.
   */
  branch_name: string;
  channel_id: string;
  links: SelfContextLinks;
  repo_root: string;
  session_id: string;
  session_url: string;
}

/**
 * Result of `sessions.archive`. `kind` is `"session"` for a real session or
 * `"launch_attempt"` when the id named a reservation that never became one
 * (its reserved runtime is torn down and the automation row kept as history).
 */
export interface SessionArchiveResult {
  archived: boolean;
  branch: string;
  kind: string;
  warnings: string[];
}

/**
 * One structured catch-up for an agent resuming a session. Consumers render
 * this for terminals or return it directly over MCP.
 */
export interface SessionCatchupView {
  artifacts: ArtifactMeta[];
  attention: string;
  branch_id: string;
  channel: ChannelView | null;
  goal: string;
  issues: IssueView[];
  next_actions: string[];
  recent_events: Event[];
  session_id: string;
  status_message: string;
}

/**
 * Result of `sessions.chat`: a page of the journal plus the composer state
 * needed to render it.
 */
export interface SessionChatView {
  blocks: ChatBlockView[];
  /**
   * The permission posture captured when the in-flight turn started; may
   * differ from a live `current_mode` selection, which applies next turn.
   */
  effective_mode: string | null;
  /** The turn currently in flight, if any (ACP only). */
  live_turn: number | null;
  metadata: AcpMetadataView;
  older_cursor: ChatCursorView | null;
  pending_prompt: string | null;
}

export type SessionCreatorFilter = 'mine' | 'ops' | 'mine-and-ops' | 'other-users';

export interface SessionCustomMcpView {
  digest: string;
  group: string;
  identity: string;
  revision: number;
  server_name: string;
  tools: string[];
}

/** Result of `sessions.files`. */
export interface SessionFilesView {
  files: string[];
}

/**
 * One explicit repository grant layered onto a session's launch-time GitHub
 * policy. GitHub App credentials currently expose one reviewed write policy;
 * `none` is accepted only as the mutation that revokes a grant.
 */
export interface SessionGithubAccessView {
  granted_at: string;
  granted_by: string;
  mode: string;
  repository: string;
}

/** One complete group order in an atomic layout restore. */
export interface SessionGroupOrderReq {
  group_id: string;
  session_ids: string[];
}

/** An ordered, flat group inside one session space. */
export interface SessionGroupView {
  /** Viewer-specific disclosure preference; membership/order remain shared. */
  collapsed: boolean;
  id: string;
  name: string;
  rank: number;
  /**
   * Canonically ordered session ids, including archived rows. Fleet views
   * decide whether to project active work or History.
   */
  session_ids: string[];
  space_id: string;
  system_key: string | null;
}

/**
 * Result of `sessions.ide_info`: whether the embedded editor is enabled and
 * runnable on this host.
 */
export interface SessionIdeInfoView {
  available: boolean;
  enabled: boolean;
  idle_timeout_secs: number;
}

/** Result of `POST /api/sessions/{id}/interrupt`. */
export interface SessionInterruptResult {
  interrupted: boolean;
}

export type SessionLayoutItemKind = 'space' | 'group';

/** Complete shared session layout at one optimistic-concurrency revision. */
export interface SessionLayoutView {
  defaults: SessionPlacementDefaultView[];
  revision: number;
  spaces: SessionSpaceView[];
}

/** Source-redacted MCP audit policy returned on ordinary session views. */
export interface SessionMcpPolicyView {
  capability_sets: McpCapabilitySetView[];
  custom_servers: SessionCustomMcpView[];
  selection: McpAccess;
}

/** Result of `sessions.mode`. */
export interface SessionModeResult {
  mode_id: string;
}

/** One configurable default-placement selector. */
export interface SessionPlacementDefaultView {
  group_id: string;
  selector_kind: SessionPlacementSelectorKind;
  selector_value: string;
}

export type SessionPlacementSelectorKind = 'origin' | 'profile' | 'watch';

/** One session's canonical position in the shared Spaces → Groups layout. */
export interface SessionPlacementView {
  group_id: string;
  group_name: string;
  group_system_key: string | null;
  rank: number;
  session_id: string;
  space_id: string;
  space_name: string;
}

/**
 * Result of `GET /api/sessions/{id}/preview`: the session's terminal pane (or,
 * for an ACP session, its recent journal) rendered as plain text.
 */
export interface SessionPreviewResult {
  screen: string;
}

export type SessionSearchAttention = 'needs' | 'ok' | 'attention' | 'blocked';

export type SessionSearchStatus = 'created' | 'running' | 'orphaned' | 'done' | 'error' | 'archived';

/** Result of `POST /api/sessions/{id}/send`. */
export interface SessionSendResult {
  /**
   * Whether the prompt was queued behind an active turn. Set only for an ACP
   * session; `null` for a terminal session, which has no queue.
   */
  queued: boolean | null;
  sent: boolean;
  submitted: boolean;
  /**
   * The turn the prompt belongs to. Set only for an ACP session; `null`
   * for a terminal session.
   */
  turn: number | null;
}

/** A top-level shared fleet space. */
export interface SessionSpaceView {
  groups: SessionGroupView[];
  id: string;
  name: string;
  rank: number;
  system_key: string | null;
}

/**
 * Compact session projection returned by `GET /api/sessions/summary`.
 *
 * This is the polling/search contract for fleet indexes. A client follows with
 * `GET /api/sessions/{id}` only when it opens a session or discloses the row's
 * complete context.
 */
export interface SessionSummaryView {
  branch: BranchSummaryView;
  class: string;
  created_at: string;
  created_by: string | null;
  github_issue: GithubIssueRef | null;
  github_repo: string | null;
  id: string;
  last_activity_at: string;
  origin: string;
  parent_id: string | null;
  parent_session_id: string | null;
  placement: SessionPlacementView | null;
  profile: string;
  status: string;
  tracking_issue: number | null;
  transition: SessionTransitionView | null;
  usage: AcpUsage | null;
}

export interface SessionTransitionView {
  /** Stable operation name: currently `archiving` or `adopting`. */
  kind: string;
  /** ISO timestamp at which this operation claimed the session. */
  started_at: string;
  /** Human-readable current stage, suitable for direct UI presentation. */
  step: string;
}

/** Result of `sessions.url`. */
export interface SessionUrlView {
  url: string;
}

/** Session-scoped view returned by the `/api/sessions[/...]` endpoints. */
export interface SessionView {
  /** The agent's own on-disk ACP session id for an `acp` session, or `null`. */
  acp_session_id: string | null;
  agent_kind: string;
  branch: BranchView;
  /**
   * Machine tier: `"interactive"` or `"automation"`. Both appear in the
   * normal fleet; the class remains useful for policy and compatibility
   * filters.
   */
  class: string;
  created_at: string;
  /**
   * The principal (username) that launched this session — attribution for the
   * shared team board. `null` for engine-created warm watch sessions and rows
   * that predate the column. A tracking/UX field,
   * not a security boundary: the fleet stays co-owned by everyone authenticated.
   */
  created_by: string | null;
  /**
   * The current ACP mode id (gating posture: `bypassPermissions`, `auto`,
   * `acceptEdits`, `default`, `plan`), or `null` for a terminal session /
   * before one is set.
   */
  current_mode: string | null;
  effort: string;
  /**
   * GitHub issue linked to this session's explicit work item, if any. This is
   * separate from `branch.github`, which is the pull request created by the
   * work. The compatibility work item remains the source of truth for edits.
   */
  github_issue: GithubIssueRef | null;
  github_repo: string | null;
  id: string;
  last_activity_at: string;
  /** Resolved launch permission posture, immutable for this session. */
  launch_mode: string;
  /** Exact, source-redacted MCP capability snapshot stamped at launch. */
  mcp_policy: SessionMcpPolicyView;
  model: string;
  /** Monotonic lifecycle/goal mutation generation used to fence handoff. */
  mutation_revision: number;
  /**
   * How this session came to exist: `"user"` (hand-launched), `"agent"`
   * (delegated by another session), `"github"` / `"slack"` (chat triggers),
   * `"watch"` (engine infrastructure). Stamped once at create.
   */
  origin: string;
  /**
   * Branch id of the session that **launched** this one — the parent in the
   * dashboard's session tree — or `null` for a top-level session.
   */
  parent_id: string | null;
  /**
   * Exact immutable session id of the launcher. New rows always stamp this.
   * `parent_id` is retained for backward compatibility with older sessions.
   */
  parent_session_id: string | null;
  /**
   * Legacy compatibility read derived from canonical placement. `"parked"`
   * means the session currently belongs to a system `Later` group; all
   * other placements read as `null`.
   */
  park: string | null;
  /** The session's one canonical, operator-controlled fleet location. */
  placement: SessionPlacementView | null;
  /** Immutable environment precedence accepted at launch. */
  policy_strict: boolean;
  /** Named launch posture selected when this session was created. */
  profile: string;
  /**
   * Stable identity of the profile lifetime accepted at launch. Zero means
   * an upgraded row whose same-name relationship could not be proven.
   */
  profile_lifetime: number;
  /** Revision of the profile whose non-secret policy was stamped at launch. */
  profile_revision: number;
  /**
   * Execution backend: `"terminal"` (a PTY + interactive TUI) or `"acp"` (a
   * headless adapter driven over the Agent Client Protocol). Terminal-backend
   * and older rows read as `"terminal"`.
   */
  protocol: string;
  /**
   * Canonical server-resolved launch snapshot. Older sessions created before
   * the launch-composition contract expose `null`.
   */
  resolved_launch: ResolvedLaunchView | null;
  /**
   * Legacy compatibility read: the canonical zero-based rank within the
   * current group. It is normalized after every move and has no meaning
   * across groups.
   */
  sort_order: number | null;
  status: string;
  term_session: string;
  /** Optional metadata-agent state for the task label. */
  title_generation: TitleGenerationView;
  /**
   * An explicit claimed/imported compatibility work item. Ordinary sessions
   * coordinate through their same-id channel and leave this `null`.
   */
  tracking_issue: number | null;
  transition: SessionTransitionView | null;
  /** Completed agent turns on this session. */
  turn_count: number;
  updated_at: string;
  /**
   * The latest context-window usage reported by the current ACP provider, or
   * `null` before it reports (and immediately after a provider handoff).
   */
  usage: AcpUsage | null;
  work_dir: string;
}

export type SettingKind = 'string' | 'text' | 'int' | 'bool' | 'enum';

export type SettingSource = 'default' | 'deployment' | 'runtime';

/**
 * One registered setting with all registry metadata and its effective value,
 * as both `settings.get` and `settings.patch` return it.
 */
export interface SettingView {
  default: string;
  deployment_value: string | null;
  description: string;
  group: string;
  is_default: boolean;
  key: string;
  kind: SettingKind;
  label: string;
  options: string[];
  source: SettingSource;
  value: string;
}

/** The envelope both `settings.get` and `settings.patch` return. */
export interface SettingsEnvelope {
  settings: SettingView[];
}

/**
 * Result of `shell.restart`: the operator shell's process was replaced, so its
 * working directory and environment are whatever a fresh login gets.
 */
export interface ShellRestartResult {
  restarted: boolean;
}

/**
 * Who may launch a session from Slack: the whole workspace, or a listed set
 * of user ids (`users` is empty for the workspace-wide mode).
 */
export interface SlackAccessView {
  mode: string;
  users: string[];
}

/**
 * The identity `auth.test` resolves, when a bot token is configured.
 * `error` is set instead of the rest when the probe itself fails.
 */
export interface SlackIdentityView {
  error: string | null;
  team_id: string | null;
  /** `"bot"` or `"user"`, depending on which kind of token is configured. */
  token_kind: string | null;
  user_id: string | null;
}

/**
 * What the Socket Mode supervisor has seen, for the Connections pane and the
 * logs.
 */
export interface SlackSocketView {
  app_id: string | null;
  connected_at: string | null;
  events_received: number;
  followups_routed: number;
  last_error: string | null;
  last_event_at: string | null;
  last_skip: string | null;
  last_skip_at: string | null;
  sessions_launched: number;
  state: string;
}

/**
 * One Slack thread, as an automation caller names it. `channel` is a Slack
 * channel id (`C…`/`G…`/`D…`, never a `#name`) and `thread_ts` the message `ts`
 * of the thread's root. The workspace is loom's own — a caller cannot address
 * another team — and the bot token stays server-side, so this is a destination
 * request, not a capability the caller holds.
 */
export interface SlackThreadRef {
  channel: string;
  thread_ts: string;
}

/** One desired tag in `PUT /api/sessions/{id}/tags`. */
export interface TagInput {
  key: string;
  /** One-line reason accompanying the tag. */
  note?: string;
  value: string;
}

/** One exact `(key, value)` tag to clear in the same atomic replacement. */
export interface TagMatch {
  key: string;
  value: string;
}

/**
 * One tag on a branch, as the API exposes it. A `(key, value)` annotation with
 * a reason, author, and timestamp. The well-known keys are `attention` (the
 * agent's self-report) and `triage` (a watch's assessment); any other key
 * is a free-form, quiet pill. Absence is the calm state — there is no `ok` tag.
 */
export interface TagView {
  key: string;
  note: string;
  set_at: string;
  set_by: string;
  value: string;
}

/**
 * One detached background task's lifecycle, as `GET /api/tasks` exposes it —
 * currently the GitHub `@loom` trigger launches, which run off the webhook
 * request so a slow clone can't blow GitHub's delivery timeout. Human-only
 * self-service debugging (Settings → Diagnostics), same as the log endpoints:
 * a task label names a repo/issue an operator can act on.
 */
export interface TaskView {
  /** Outcome detail: a session id, `forwarded`, or an error message. */
  detail: string;
  finished_at: string | null;
  id: number;
  /** A coarse category, e.g. `github-trigger`. */
  kind: string;
  /** A human label, e.g. `marin-community/marin#6823 (@rjpower)`. */
  label: string;
  started_at: string;
  /** `running` | `done` | `error`. */
  state: string;
}

/**
 * A discussion thread on an artifact span: its anchor, status, and comments
 * (oldest first), as the GET/POST thread endpoints expose it.
 */
export interface ThreadDto {
  anchor: AnchorDto;
  /** The artifact revision the anchor was taken from. */
  base_rev: number;
  comments: CommentDto[];
  created_at: string;
  id: number;
  resolved_at: string | null;
  /** `open` | `resolved` | `orphaned`. */
  status: string;
}

export interface TitleGenerationView {
  enabled: boolean;
  /**
   * `idle`, `running`, `generated`, `protected`, `disabled`, `unavailable`,
   * `stale`, or `failed`.
   */
  status: string;
}

/**
 * One API token's non-secret metadata (`GET /api/auth/tokens`). The secret
 * itself is only ever returned once, in [`CreatedTokenView`].
 */
export interface TokenView {
  created_at: string;
  expires_at: string | null;
  id: string;
  last_used_at: string | null;
  name: string;
  /** The non-secret leading slice, e.g. `loom_AbCd…`, to tell tokens apart. */
  prefix: string;
}

/** One personal preference with its deployment-wide inherited value. */
export interface UserPreferenceView {
  description: string;
  inherited_value: string;
  is_overridden: boolean;
  key: string;
  kind: SettingKind;
  label: string;
  options: string[];
  value: string;
}

/**
 * Effective personal preferences returned by `preferences.get` and
 * `preferences.patch`.
 */
export interface UserPreferencesEnvelope {
  preferences: UserPreferenceView[];
}

export type UserRole = 'admin' | 'user';

/**
 * One approved operator (`GET /api/auth/users`). The password hash is never
 * exposed — only whether one is set.
 */
export interface UserView {
  created_at: string;
  github_login: string | null;
  has_password: boolean;
  role: UserRole;
  username: string;
}

/** Result of `DELETE /api/watches/{id}`. */
export interface WatchDeleteResult {
  deleted: boolean;
  id: string;
}

/**
 * Result of firing a watch round on demand (`POST /api/watches/{id}/run`):
 * the round's id and its closed outcome, re-read from the run history once
 * the round finishes.
 */
export interface WatchRunResult {
  /**
   * `ok|noop|skipped|error`, or empty if the round row could not be
   * re-read.
   */
  outcome: string;
  run_id: number;
  summary: string;
}

/**
 * One round in a watch's history (the audit trail), with `actions`
 * parsed back into JSON for a UI to render. The `stdout`/`stderr`/`exit_code`/
 * `duration_ms` fields are the captured execution log — what the script printed
 * and returned — surfaced so a run page shows exactly what happened.
 */
export interface WatchRunView {
  /** The JSON array of marks / nudges / would-dos the round recorded. */
  actions: unknown;
  /** Wall-clock the program ran, in milliseconds. */
  duration_ms: number | null;
  /** The interpreter's exit status, or `null` when it never spawned / timed out. */
  exit_code: number | null;
  finished_at: string | null;
  id: number;
  outcome: string;
  started_at: string;
  /** A tail of the script's standard error. */
  stderr: string;
  /** A tail of the script's standard output. */
  stdout: string;
  summary: string;
  /**
   * The normalized event that woke the round (`cron` / `manual` / e.g.
   * `pr.merged`).
   */
  trigger_event: string;
  trigger_reason: string;
}

/**
 * One watch, as the API exposes it. The JSON-bearing columns
 * (`trigger`, `scope`, `params`) are returned as **parsed** structured JSON so
 * a UI never re-parses strings; `capabilities` is a real array; the rest is the
 * stored definition plus its schedule bookkeeping.
 */
export interface WatchView {
  /**
   * The granted capability set (the intervention ladder). `observe` is
   * implicit; the rest are explicit grants.
   */
  capabilities: string[];
  cooldown_secs: number;
  created_at: string;
  effort: string;
  enabled: boolean;
  id: string;
  /**
   * The most recent round's outcome (`ok|noop|skipped|error`), or `null` if
   * it has never run — the at-a-glance health a list view shows.
   */
  last_outcome: string | null;
  last_run_at: string | null;
  model: string;
  name: string;
  next_run_at: string | null;
  /** Stock-program parameters (e.g. the judgement `prompt`), parsed. */
  params: unknown;
  /**
   * Automation-safe launch profile used for agent judgements and warm
   * sessions.
   */
  profile: string;
  /**
   * `builtin:<name>` for a stock program, or an absolute path under
   * `~/.weaver/watches/` for a custom one.
   */
  program: string;
  /** The fleet query a round surveys, parsed: `{attention?, repo?}`. */
  scope: unknown;
  /**
   * The program's lookaside state, parsed — its scratch memory carried across
   * rounds (e.g. a backoff watcher's per-session attempt counts). `{}` when
   * the program keeps none.
   */
  state: unknown;
  /** The event-match predicate, parsed: `{cron|every|event|level|repo}`. */
  trigger: unknown;
  updated_at: string;
  /**
   * The one-shot dynamic re-trigger time a round armed (`wake_in`), or `null`.
   * Distinct from `next_run_at` (the cron cadence): a self-scheduled backoff
   * recheck a watch set for itself.
   */
  wake_at: string | null;
  /**
   * Warm mode (`params.warm`): the engine keeps one long-lived, fleet-hidden
   * session for this watch so it has across-round memory.
   */
  warm: boolean;
  /**
   * The id of that warm session once the engine has created it, else `null`.
   * Its live terminal is reachable from the watch's detail page (the
   * session is hidden from the fleet listing).
   */
  warm_session_id: string | null;
}

// -- Per-operation input and output ---------------------------------------

/** Define a new custom agent — a name, a label, and a shell command per launch stage — so it appears in the picker beside the builtin `claude`/`codex` without a code change. */
export interface AgentsCustomCreateInput {
  /** The display name shown in the agent picker. */
  label?: string;
  /**
   * The fresh-session launch command; the goal is appended as an
   * argument.
   */
  launch?: string;
  /**
   * The new agent's unique id. Must not shadow a builtin (`claude`,
   * `codex`) or the retired `concierge` name.
   */
  name: string;
  /** Execution backend: `terminal` (the default) or `acp`. */
  protocol?: string;
  /**
   * Whether the agent fires loom's lifecycle hooks (working / idle /
   * attention signals).
   */
  reports_status?: boolean;
  /** The adopt/resume command (no goal). Blank reuses `launch`. */
  resume?: string;
  /**
   * Shell run in the worktree before launch — the "installing hooks"
   * stage.
   */
  setup?: string;
}

/** Remove a custom agent. Removing an absent name is a no-op. Sessions already launched with it are unaffected. */
export interface AgentsCustomDeleteInput {
  /** The custom agent's name. */
  name: string;
}

/** Replace an existing custom agent's definition. The name is immutable; a builtin or unknown name is rejected. */
export interface AgentsCustomUpdateInput {
  /** The display name shown in the agent picker. */
  label?: string;
  /**
   * The fresh-session launch command; the goal is appended as an
   * argument.
   */
  launch?: string;
  /** The custom agent's name. */
  name: string;
  /** Execution backend: `terminal` (the default) or `acp`. */
  protocol?: string;
  /** Whether the agent fires loom's lifecycle hooks. */
  reports_status?: boolean;
  /** The adopt/resume command (no goal). Blank reuses `launch`. */
  resume?: string;
  /** Shell run in the worktree before launch. */
  setup?: string;
}

/** List available agent runtimes: builtins, operator-defined custom agents, and the configured default. */
export interface AgentsListInput {
}

/** Run a one-shot ACP prompt through a registered agent runtime and return its text — the judgement-call primitive watch programs call. */
export interface AgentsOneshotInput {
  /** Registered ACP runtime. Empty keeps the built-in Claude runtime. */
  agent?: string;
  /**
   * Reasoning effort override advertised by the runtime; empty keeps its
   * ACP default.
   */
  effort?: string;
  /** Model override advertised by the runtime; empty keeps its ACP default. */
  model?: string;
  /**
   * Optional launch profile. When set, its runtime and policy are
   * authoritative; model and effort remain optional per-call overrides.
   */
  profile?: string;
  /** The prompt to run. */
  prompt: string;
}

export interface AgentsOneshotOutput {
  /**
   * `null` when the adapter is absent or fails — callers degrade to their
   * own deterministic fallback rather than seeing an error.
   */
  output: string | null;
}

/** Delete an artifact and its complete revision history. */
export interface ArtifactsDeleteInput {
  /** The artifact's name. */
  name: string;
  /**
   * When true, delete the repository-shared artifact. By default, delete
   * this branch's own copy.
   */
  repo?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Read one artifact or immutable revision. */
export interface ArtifactsGetInput {
  /** The artifact's name. */
  name: string;
  /**
   * When true, read the repository-shared artifact. By default, resolve
   * this branch's own copy first.
   */
  repo?: boolean;
  /** Select an immutable past revision instead of the latest. */
  rev?: number | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List immutable artifact revisions. */
export interface ArtifactsHistoryInput {
  /** The artifact's name. */
  name: string;
  /**
   * When true, list the repository-shared artifact's history. By default,
   * list this branch's own copy.
   */
  repo?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List branch and repository-scoped artifacts. */
export interface ArtifactsListInput {
  /**
   * When true, list every artifact in the repository. By default, list
   * only this branch's own artifacts and the repository-shared ones.
   */
  repo?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** An image artifact's decoded bytes, for an `<img src>`. */
export interface ArtifactsRawInput {
  /** The artifact's name. */
  name?: string;
  /** Pin an immutable past revision instead of the latest. */
  rev?: number | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Start or reply to an artifact review thread. */
export interface ArtifactsThreadsCommentInput {
  /** The comment text. */
  body: string;
  /** The artifact's name. */
  name: string;
  /**
   * Start a new thread or reply to one. On the command line this takes a
   * JSON object, because a tagged union is not a flag.
   */
  target: CommentTarget;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List anchored artifact review threads. */
export interface ArtifactsThreadsListInput {
  /** The artifact's name. */
  name: string;
  /**
   * When true, list only unresolved threads. By default, include all threads.
   * Resolved threads appear collapsed in the dashboard, not hidden.
   */
  open_only?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Resolve an artifact review thread. */
export interface ArtifactsThreadsResolveInput {
  /** The artifact's name. */
  name: string;
  /** The thread to resolve. */
  thread_id: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** The externally-visible dashboard deep-link for an artifact. */
export interface ArtifactsUrlInput {
  /** The artifact's name. */
  name: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Create an artifact or append a guarded revision. */
export interface ArtifactsWriteInput {
  /**
   * Optimistic-concurrency guard: `0` guards creation; a later revision
   * number rejects a stale edit instead of silently overwriting it.
   */
  base_rev?: number | null;
  /**
   * The artifact body. On the command line this names a file, or `-`/omitted
   * to read stdin.
   */
  content: string;
  /**
   * Content kind, e.g. `markdown` or `image`.
   *
   * When omitted, the artifact keeps its current kind. This must be optional
   * because a default value would silently change existing `plan` or `image`
   * artifacts to markdown on every update that omits this field.
   */
  kind?: string | null;
  /** The artifact's name. */
  name: string;
  /**
   * Write the repository-shared artifact instead of this branch's own
   * copy.
   */
  repo?: boolean;
  /**
   * Display title. Defaults to the existing title, or the name for a new
   * artifact.
   */
  title?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Mint a short-lived automation-only token for a given subject. */
export interface AuthAutomationTokenInput {
  /** Profiles the token may launch runs under. */
  profiles?: string[];
  /** Stable identity recorded on runs launched with this token. */
  subject: string;
  /** Lifetime in seconds. */
  ttl_secs?: number;
}

/** Exchange a workload-identity OIDC token for a short-lived automation token, per a mapping an admin registered with `auth.federations.create`. */
export interface AuthFederateInput {
  /** The workload-identity OIDC token to exchange. */
  token: string;
}

/** Register (or idempotently reconcile) a workload-identity federation mapping — the trust relationship `auth.federate` exchanges an OIDC token against. */
export interface AuthFederationsCreateInput {
  audience: string;
  event_name?: string | null;
  issuer?: string;
  /**
   * Stable operator-owned identity used for idempotent reconciliation.
   * When omitted, one is derived from the identity fields below.
   */
  name?: string | null;
  /** Profiles a token minted through this mapping may launch runs under. */
  profiles?: string[];
  provider?: string;
  ref_pattern?: string | null;
  repository_id?: string | null;
  /** Exact verified Google service-account email. */
  service_account?: string | null;
  /** Stable, bounded audit label copied into Loom automation credentials. */
  service_tag?: string;
  /** Exact numeric OIDC subject for Google workload identities. */
  subject?: string | null;
  workflow_ref?: string | null;
}

/** List the registered workload-identity federation mappings. */
export interface AuthFederationsListInput {
}

/** Remove a workload-identity federation mapping. */
export interface AuthFederationsRemoveInput {
  /** The mapping id (from `federation ls`). */
  id: string;
}

/** Read the GitHub sign-in / App setup (secret withheld). */
export interface AuthGithubConfigGetInput {
}

/** Set the GitHub sign-in OAuth client id (and, optionally, its secret). */
export interface AuthGithubConfigSetInput {
  client_id: string;
  /**
   * Write-only: omit to leave the stored secret untouched, or pass an
   * empty string to clear it.
   */
  client_secret?: string | null;
}

/** Whether the caller has a personal GitHub token on file, and when it last changed. */
export interface AuthGithubTokenGetInput {
}

/** Remove the caller's personal GitHub token. */
export interface AuthGithubTokenRemoveInput {
}

/** Set the caller's personal GitHub token. Loom selects it for ordinary interactive sessions this user launches; restricted sessions never use it. */
export interface AuthGithubTokenSetInput {
  /**
   * The token value. On the command line this names a file, or `-`/omitted
   * to read stdin, so the secret need not sit in shell history.
   */
  token: string;
}

/** Exchange a username and password for a signed-in session. */
export interface AuthLoginInput {
  password: string;
  username: string;
}

/** End the caller's signed-in session. */
export interface AuthLogoutInput {
}

/** Who the caller is, and which sign-in methods the server offers. */
export interface AuthMeInput {
}

/** Set or change the caller's own password. */
export interface AuthSetPasswordInput {
  /** The new password (minimum 8 characters). */
  new_password: string;
}

/** Mint a new personal API token. The plaintext is returned once — the server keeps only a hash. */
export interface AuthTokensCreateInput {
  /** Optional lifetime in days; omitted or non-positive never expires. */
  expires_in_days?: number | null;
  /** A label to recognise the token by (e.g. `github-actions`). */
  name: string;
}

/** List the caller's own personal API tokens (metadata only; secrets are never returned). */
export interface AuthTokensListInput {
}

/** Revoke one of the caller's own personal API tokens. */
export interface AuthTokensRevokeInput {
  /** The token id (from `token ls`). */
  id: string;
}

/** Add a new operator to the approved allowlist. */
export interface AuthUsersCreateInput {
  /** The GitHub login allowed to sign in as this operator. */
  github_login?: string | null;
  /**
   * A password, if this operator should also be able to sign in with one.
   * At least one of `github_login` or `password` is required.
   */
  password?: string | null;
  /** `admin` or `user`. */
  role?: UserRole;
  username: string;
}

/** List the approved operators. */
export interface AuthUsersListInput {
}

/** Remove an approved operator. A caller may not remove themself. */
export interface AuthUsersRemoveInput {
  username: string;
}

/** Change an operator's role. Existing cookies and personal tokens observe the change on their next request. */
export interface AuthUsersSetRoleInput {
  /** `admin` or `user`. */
  role: UserRole;
  username: string;
}

/** Append a raw event row to a branch's log — the escape hatch for an event kind with no dedicated mutating route of its own. */
export interface BranchesEventsCreateInput {
  /** Arbitrary event payload. */
  data?: unknown;
  /** The event kind, e.g. an agent hook name. */
  kind: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List recent durable events on a branch (newest first, last 200 entries). */
export interface BranchesEventsListInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Inspect one branch. */
export interface BranchesGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List work items claimed by this branch — the session's working set. */
export interface BranchesIssuesListInput {
  /** Include closed work items. */
  all?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List every branch loom is tracking (fleet-wide, unfiltered). */
export interface BranchesListInput {
}

/** Post a message from this branch's session back to a Slack thread. */
export interface BranchesSlackReplyInput {
  /** Dedupe key so a retried send doesn't double-post. */
  idempotency_key?: string | null;
  /** The message text. */
  text: string;
  /** Delivered thread to reply in (optional). */
  thread?: SlackThreadRef | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Set the branch's attention level and current-state message in one call. */
export interface BranchesStatusSetInput {
  /** The attention level. */
  level: 'ok' | 'attention' | 'blocked';
  /**
   * The current-state message shown alongside the level. Absent/empty
   * leaves the previous message in place.
   */
  message?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Remove one free-form tag from a branch — the branch-scoped twin of `sessions.tags.delete`. */
export interface BranchesTagsDeleteInput {
  /** Who is clearing it (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The tag key to remove. */
  key: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Set one free-form tag on a branch — the branch-scoped twin of `sessions.tags.set`, for a target with no live session bound to it (a finished session, or an id naming another branch entirely). */
export interface BranchesTagsSetInput {
  /** Who is setting it (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The tag key. */
  key: string;
  /** One-line reason accompanying the tag. */
  note?: string;
  /** The tag value. */
  value: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Update a branch's title, goal, or current-state description. */
export interface BranchesUpdateInput {
  /** The agent's current-state message. */
  description?: string | null;
  /** Required with `title`. */
  expected_title?: string | null;
  /** Required with `title`. */
  expected_title_provenance?: string | null;
  goal?: string | null;
  title?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Archive a custom channel. */
export interface ChannelsArchiveInput {
  /** A visible channel id. */
  channel: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List a channel's external delivery bindings: subscribed session inboxes, plus the originating Slack thread if the branch is wired to one. */
export interface ChannelsBindingsListInput {
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Open a custom durable channel. */
export interface ChannelsCreateInput {
  /** The new channel's name. */
  name: string;
  /** Optional topic description. */
  topic?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Inspect one channel and its delivery bindings. */
export interface ChannelsGetInput {
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** List visible durable channels and their unread state. */
export interface ChannelsListInput {
  /** Include archived channels. */
  archived?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Append and deliver a durable channel message. */
export interface ChannelsMessagesCreateInput {
  /** The message body. */
  body: string;
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /** Retry-safe key scoped to the channel. */
  idempotency_key?: string | null;
  /** `message`, `status`, or `result`. */
  kind?: 'message' | 'status' | 'result';
  /** Arbitrary structured payload alongside the body. */
  payload?: unknown;
  /** Reply to an existing message in this channel. */
  reply_to?: string | null;
  /** `normal`, `attention`, or `blocked`. */
  urgency?: 'normal' | 'attention' | 'blocked';
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Read a channel's message history, advancing the read marker unless peeking. */
export interface ChannelsMessagesListInput {
  /** Only return items after this sequence number. */
  after?: number;
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /**
   * Restrict to these message kinds (`goal`, `message`, `status`,
   * `result`, `system`).
   */
  kinds?: ('goal' | 'message' | 'status' | 'result' | 'system')[];
  /** Maximum number of items to return. */
  limit?: number;
  /** Read without advancing this session's read marker. */
  peek?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Acknowledge a channel through a sequence number. */
export interface ChannelsReadMarkerSetInput {
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /**
   * Mark read through this sequence; omission advances through the
   * latest message.
   */
  seq?: number | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Set how a session follows a channel. */
export interface ChannelsSubscriptionSetInput {
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /** `observe` or `deliver`. */
  mode?: 'observe' | 'deliver';
  /** Subscribe this descendant session instead of the caller. */
  session?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Wait for the next matching channel message. */
export interface ChannelsWaitInput {
  /**
   * Wait for items after this sequence; omission starts from the
   * channel's latest known message.
   */
  after?: number | null;
  /**
   * A visible channel id. Empty means this session's own channel,
   * resolved server-side.
   */
  channel?: string;
  /** Wake only for this message kind, e.g. `result`. */
  kind?: 'goal' | 'message' | 'status' | 'result' | 'system' | null;
  /** Seconds to wait before giving up. */
  timeout?: number;
  /** Wake only for `attention` or `blocked` urgency. */
  urgent?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Reconcile the runtime resources declared by a deployment stack: settings, launch profiles, and federation mappings. */
export interface DeploymentReconcileInput {
  /** Trusted GitHub Actions OIDC workflow mappings this stack declares. */
  federations?: FederationReq[];
  /**
   * Named profiles this stack declares, each with its write-only
   * environment.
   */
  profiles?: DeploymentProfileReq[];
  /**
   * Remove previously deployment-managed resources omitted from this
   * request.
   */
  prune?: boolean;
  /**
   * Organization defaults for registered runtime settings. Live database
   * values remain a higher-precedence override.
   */
  settings?: Record<string, DeploymentSettingValue>;
}

/** The aggregated fleet diagnostics snapshot: session/profile capacity, automation run health, migration state, and federation mappings. */
export interface DiagnosticsGetInput {
}

/** Build and process identity for a human operator's debug panel: which version and image are running, and since when. */
export interface DiagnosticsStatusInput {
}

/**
 * A small "what am I looking at" status blob for the debug panel: build and
 * image identity plus process identity, so both deploys and restarts are
 * attributable.
 */
export interface DiagnosticsStatusOutput {
  build_profile: string;
  build_revision: string;
  /**
   * Digest-pinned image reference when a container deployment supplies
   * one.
   */
  image: string | null;
  pid: number;
  /** When this process started capturing logs (≈ process start), RFC3339. */
  started_at: string;
  version: string;
}

/** Subscribe to one or more event topics over a single SSE connection. */
export interface EventsStreamInput {
  /**
   * Comma-separated topic list: `layout`, `logs`, `session:<key>`,
   * `chat:<key>`. Empty parks the connection on keep-alive.
   */
  topics?: string;
}

/** Apply one action atomically to a set of work items. */
export interface IssuesActionsInput {
  /**
   * The action to apply — `close`, `reopen`, `delete`, `tag`, or `untag`.
   * On the command line this takes a JSON object, because a tagged union is
   * not a flag.
   */
  action: IssueAction;
  /** The work items to act on. Either every id succeeds or none does. */
  ids?: number[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Create an unclaimed repository backlog item. */
export interface IssuesBacklogCreateInput {
  /** Optional detail. */
  body?: string;
  /** Link the item to an existing GitHub issue number. */
  github_issue?: number | null;
  /**
   * Tags to apply in the same transaction as the insert.
   *
   * Atomic on purpose: the create-issue form stages tags before the item
   * exists, and applying them afterwards would leave a window where the board
   * shows an untagged item — or, if the second call fails, keeps it untagged.
   */
  tags?: IssueTagInput[];
  /** One-line summary of the work. */
  title: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  source_branch?: string;
}

/** Every work item across every repository — the dashboard's board. */
export interface IssuesBoardInput {
  /** Include closed work items. */
  all?: boolean;
  /**
   * Include items claimed by an automation-class session's branch. Defaults
   * to `false` — the board shows the work of the interactive fleet, not the
   * trackers its machinery opens for itself.
   */
  automation?: boolean;
}

/** Close one or more work items atomically. */
export interface IssuesCloseInput {
  /**
   * One or more Loom work-item ids. Applied atomically: either every id
   * succeeds or none does.
   */
  ids?: number[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Create a work item claimed by this session's branch. */
export interface IssuesCreateInput {
  /** Optional detail. */
  body?: string;
  /** Link the item to an existing GitHub issue number. */
  github_issue?: number | null;
  /** One-line summary of the work. */
  title: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  branch?: string;
}

/** Permanently delete one or more work items atomically. */
export interface IssuesDeleteInput {
  /**
   * One or more Loom work-item ids. Applied atomically: either every id
   * succeeds or none does.
   */
  ids?: number[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Inspect one work item and the status of the branch working it. */
export interface IssuesGetInput {
  /** A Loom work-item id. */
  id: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** List current-session and repository work items. */
export interface IssuesListInput {
  /** Include closed work items. */
  all?: boolean;
  /** List only unclaimed backlog items — those no branch has picked up. */
  backlog?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Reopen one or more closed work items atomically. */
export interface IssuesReopenInput {
  /**
   * One or more Loom work-item ids. Applied atomically: either every id
   * succeeds or none does.
   */
  ids?: number[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Remove one free-form tag from a work item. */
export interface IssuesTagsDeleteInput {
  /** A Loom work-item id. */
  id: number;
  /** The tag key to remove. */
  key: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Set one free-form tag on a work item. */
export interface IssuesTagsSetInput {
  /** A Loom work-item id. */
  id: number;
  /** The tag key. */
  key: string;
  /** One-line reason accompanying the tag. */
  note?: string;
  /** The tag value. Use `issues tag delete` to clear a tag. */
  value: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** Edit a work item's own fields. */
export interface IssuesUpdateInput {
  /** Replace the detail body. */
  body?: string | null;
  /**
   * GitHub issue mapping as `owner/name#number`. An empty string clears the
   * mapping; omitting the field leaves it unchanged.
   */
  github?: string | null;
  /** A Loom work-item id. */
  id: number;
  /** `open` or `closed`. */
  status?: 'open' | 'closed' | null;
  /** Replace the one-line summary. */
  title?: string | null;
  /** Return the item to the unclaimed backlog. */
  unclaim?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  repo_root?: string;
}

/** A snapshot of the most recent server log lines, oldest first. */
export interface LogsListInput {
  /**
   * Most-recent lines to return. Clamped to the buffer size; defaults to
   * 500.
   */
  limit?: number | null;
}

/** Tail the server log as it is written. */
export interface LogsStreamInput {
}

/** Add an operator-authored custom MCP server. */
export interface McpsCustomCreateInput {
  description?: string;
  enabled?: boolean;
  /** Absolute identity, e.g. `/engineering/search/docs`. */
  identity: string;
  /** Display label. */
  label: string;
  /**
   * A uv Python script with PEP 723 inline dependencies. On the command
   * line this names a file, or `-`/omitted to read stdin.
   */
  source: string;
  /** Optional uv Python test script. */
  test_source?: string;
}

/** Permanently remove an operator-authored custom MCP server. */
export interface McpsCustomDeleteInput {
  /** Absolute identity, e.g. `/engineering/search/docs`. */
  identity: string;
}

/** Show one operator-authored custom MCP server's latest definition and validation state. */
export interface McpsCustomGetInput {
  /** Absolute identity, e.g. `/engineering/search/docs`. */
  identity: string;
}

/** List operator-authored custom MCP servers. */
export interface McpsCustomListInput {
}

/** Replace an operator-authored custom MCP server's definition, producing a new validated revision. */
export interface McpsCustomUpdateInput {
  description?: string;
  enabled?: boolean;
  /** Absolute identity, e.g. `/engineering/search/docs`. */
  identity: string;
  /** Display label. */
  label: string;
  /**
   * A uv Python script with PEP 723 inline dependencies. On the command
   * line this names a file, or `-`/omitted to read stdin.
   */
  source: string;
  /** Optional uv Python test script. */
  test_source?: string;
}

/** The trusted MCP registry: built-in adapters, versioned capability sets, and operator-authored custom servers. */
export interface McpsGetInput {
}

/** Show this session's effective Loom operations and external repository scope. */
export interface PermissionsEffectiveGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Explain one registered operation's actor, risk, and projections. */
export interface PermissionsExplainInput {
  /** The operation id to explain, e.g. `issues.tags.set`. */
  operation: string;
}

/** Directly grant one GitHub repository to a live session, without a prior request. */
export interface PermissionsGithubGrantInput {
  /** The `owner/repo` slug to grant write access to. */
  repository: string;
  /** The session receiving access. */
  session: string;
}

/** Invoke one fixed-target GitHub operation granted by restricted session policy. */
export interface PermissionsGithubRestrictedInvokeInput {
  /** Tool-specific arguments (`number`, optional `body`/`title`). */
  arguments: unknown;
  /** The fixed restricted-GitHub tool to invoke, e.g. `issue_comment`. */
  tool: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Revoke one explicit GitHub repository override from a live session. */
export interface PermissionsGithubRevokeInput {
  /** The `owner/repo` slug to revoke write access from. */
  repository: string;
  /** The session losing access. */
  session: string;
}

/** Mint a refreshable repository-scoped GitHub App credential for this session. */
export interface PermissionsGithubTokenInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Approve and apply a pending external-access request. */
export interface PermissionsRequestsApproveInput {
  /** Optional audit reason recorded with the decision. */
  reason?: string;
  /** The pending permission request id. */
  request: string;
}

/** Request a human-approved GitHub write-access expansion for this session. */
export interface PermissionsRequestsCreateInput {
  /** Currently only `write` is accepted. */
  mode?: 'write';
  /** Why the task needs this repository. */
  reason: string;
  /** The `owner/repo` slug to request write access to. */
  repository: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Deny a pending external-access request. */
export interface PermissionsRequestsDenyInput {
  /** Optional audit reason recorded with the decision. */
  reason?: string;
  /** The pending permission request id. */
  request: string;
}

/** List durable external-access requests for this session. */
export interface PermissionsRequestsListInput {
  /** Restrict to `pending`, `approved`, or `denied`. Omit to list all. */
  state?: 'pending' | 'approved' | 'denied' | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Get this operator's personal UI preference overrides (terminal theme, font, font size), each layered over its effective inherited value. */
export interface PreferencesGetInput {
}

/** Set or clear this operator's personal UI preferences. */
export interface PreferencesPatchInput {
  changes?: Record<string, unknown>;
}

/** Clone one profile's reviewed policy into a new insert-only profile, optionally composing its write-only environment in the same transaction. If the profile changed since the caller reviewed it, this returns a fresh preview instead of silently applying a stale composition. */
export interface ProfilesCloneInput {
  /** Copy the source's write-only environment; ignored when `environment` is present. */
  copy_environment?: boolean;
  /** Explicit write-only environment composition for the clone. */
  environment?: CloneProfileEnvironmentReq | null;
  /**
   * Revision of `source` the caller reviewed; a 409 with a fresh preview
   * means it has since changed.
   */
  expected_profile_revision: number;
  /** Resolver fingerprint from the composition the caller reviewed. */
  expected_resolver_revision: string;
  /** The new profile's name. */
  name: string;
  /** Fields to layer over the source profile for this one resolution. */
  overrides?: LaunchOverrides;
  /** The profile being cloned. */
  source: string;
  /**
   * Optional fully edited profile proposal. Omitted copies the source
   * profile's policy verbatim; source revision and environment copy
   * remain server-owned and atomic either way.
   */
  template?: ProfilesCloneNestedInput | null;
}

/** Create a named session-launch profile. */
export interface ProfilesCreateInput {
  /** Agent runtime this profile launches (e.g. `claude`, `codex`). */
  agent_kind: string;
  ambient_allowlist?: string[];
  class?: string;
  description?: string;
  /** Blank uses the runtime's own default. */
  effort?: string;
  env_clear?: boolean;
  /**
   * Repositories for which Loom may broker a short-lived GitHub App
   * token.
   */
  github_repositories?: string[];
  idle_archive_secs?: number | null;
  /**
   * Organization-owned instructions appended to this profile's opening
   * prompt for every launch origin.
   */
  instructions?: string;
  max_concurrent?: number;
  /** Provider-neutral MCP selection: `none`, `all`, or `groups`. */
  mcp_access?: McpAccess;
  /** Blank uses the runtime's own default. */
  mode?: string;
  /** Blank uses the runtime's own default. */
  model?: string;
  /** The profile's name. */
  name: string;
  prelude?: string;
  /** Blank uses the runtime's own default. */
  protocol?: string;
  restricted?: boolean;
  /** Provider-specific fallback permissions. */
  runtime_permissions?: string[];
  strict?: boolean;
  turn_budget?: number | null;
}

/** Permanently delete a named launch profile. */
export interface ProfilesDeleteInput {
  /** The profile's name. */
  name: string;
}

/** Resolve one profile's exact non-secret policy — MCP snapshot, runtime permissions, and MCP server processes — without launching a session. */
export interface ProfilesEffectiveInput {
  /** The profile's name. */
  name: string;
}

/** Remove one profile's write-only environment variable. */
export interface ProfilesEnvDeleteInput {
  /** The variable name. */
  name: string;
  /** The owning profile's name. */
  profile: string;
}

/** Set one profile's write-only environment variable from a literal value or GCP Secret Manager reference — exactly one of the two is required. */
export interface ProfilesEnvSetInput {
  /** The variable name. */
  name: string;
  /** The owning profile's name. */
  profile: string;
  /**
   * A GCP Secret Manager version resource, resolved only at launch or
   * respawn.
   */
  secret_ref?: string | null;
  /** A write-only literal. */
  value?: string | null;
}

/** Show one named launch profile. Secret environment values are never returned. */
export interface ProfilesGetInput {
  /** The profile's name. */
  name: string;
}

/** List named launch profiles. Secret environment values are never returned. */
export interface ProfilesListInput {
}

/** Replace a named session-launch profile's policy. */
export interface ProfilesUpdateInput {
  /** Agent runtime this profile launches (e.g. `claude`, `codex`). */
  agent_kind: string;
  ambient_allowlist?: string[];
  class?: string;
  description?: string;
  /** Blank uses the runtime's own default. */
  effort?: string;
  env_clear?: boolean;
  /**
   * Optimistic-concurrency guard: rejects a stale edit with a 409 and the
   * current profile instead of silently overwriting it.
   */
  expected_revision?: number | null;
  /**
   * Repositories for which Loom may broker a short-lived GitHub App
   * token.
   */
  github_repositories?: string[];
  idle_archive_secs?: number | null;
  /**
   * Organization-owned instructions appended to this profile's opening
   * prompt for every launch origin.
   */
  instructions?: string;
  max_concurrent?: number;
  /** Provider-neutral MCP selection: `none`, `all`, or `groups`. */
  mcp_access?: McpAccess;
  /** Blank uses the runtime's own default. */
  mode?: string;
  /** Blank uses the runtime's own default. */
  model?: string;
  /** The profile's name. */
  name: string;
  prelude?: string;
  /** Blank uses the runtime's own default. */
  protocol?: string;
  restricted?: boolean;
  /** Provider-specific fallback permissions. */
  runtime_permissions?: string[];
  strict?: boolean;
  turn_budget?: number | null;
}

/** List the local git branches of a repo checkout, and which has a worktree. */
export interface ReposBranchesInput {
  /** A path inside the repo checkout to list branches for. */
  cwd: string;
}

/** Remove one per-repo environment variable. Removing an absent name is a no-op. Returns the refreshed metadata list (no values). */
export interface ReposEnvDeleteInput {
  /**
   * A directory inside the repo, resolved server-side when `repo_root` is
   * omitted.
   */
  cwd?: string | null;
  /** The variable's name. */
  name: string;
  /**
   * Repo to scope to (canonical primary-worktree path). One of
   * `repo_root`/`cwd` is required.
   */
  repo_root?: string | null;
}

/** Read a repo's environment variables' metadata: names and timestamps only — values are write-only and never returned. */
export interface ReposEnvGetInput {
  /**
   * A directory inside the repo, resolved server-side when `repo_root` is
   * omitted.
   */
  cwd?: string | null;
  /**
   * Repo to scope to (canonical primary-worktree path). One of
   * `repo_root`/`cwd` is required.
   */
  repo_root?: string | null;
}

/** Upsert one per-repo environment variable. The name is validated as a shell identifier that isn't one of loom's reserved control or GitHub credential names, so it can't corrupt or shadow the launch environment. Returns the refreshed metadata list (no values). */
export interface ReposEnvSetInput {
  /**
   * A directory inside the repo, resolved server-side when `repo_root` is
   * omitted.
   */
  cwd?: string | null;
  /** The variable's name. */
  name: string;
  /**
   * Repo to scope to (canonical primary-worktree path). One of
   * `repo_root`/`cwd` is required.
   */
  repo_root?: string | null;
  /** The value to store. */
  value: string;
}

/** List the registered managed repos (the clone allowlist). */
export interface ReposListInput {
}

/** Recently-used repositories, most recent first — the launch flow's repo picker. */
export interface ReposRecentInput {
  /** Maximum repos to return (1-50); defaults to 10. */
  limit?: number | null;
}

/** Register a repo in the managed store — add it to the clone allowlist. The clone itself is lazy (it happens on first use); this just adds an entry. */
export interface ReposRegisterInput {
  /** A GitHub `owner/name` slug or a clone URL. */
  repo: string;
}

/** Check whether a worktree fork point resolves against a repo checkout, matching what a launch would fork from — fetching the revision from `origin` on demand if needed. Never touches local branches or the working tree. */
export interface ReposRevisionsValidateInput {
  /** A path inside the repo checkout to validate against. */
  cwd: string;
  /** The revision (branch, tag, or ref) to resolve. */
  revision: string;
}

/** Append an anchored feedback comment to a draft review. */
export interface ReviewsCommentsCreateInput {
  anchor: ReviewAnchorDto;
  anchor_kind: ReviewAnchorKindDto;
  body: string;
  /** Optimistic-concurrency guard on the review's draft revision. */
  expected_revision: number;
  /** The review to comment on. */
  id: number;
  /**
   * The subject version (artifact revision, or change-set version) the
   * anchor was taken against.
   */
  subject_version: string;
}

/** Remove a draft review comment. */
export interface ReviewsCommentsDeleteInput {
  /** The comment to delete. */
  comment_id: number;
  /**
   * Optimistic-concurrency guard on the review's draft revision, as
   * `loom review ls` or the previous mutation reported it.
   */
  expected_revision: number;
  /** The review the comment belongs to. */
  id: number;
}

/** Mark a comment on a submitted review resolved or unresolved. */
export interface ReviewsCommentsResolveInput {
  /** The comment to resolve or unresolve. */
  comment_id: number;
  /** The submitted review the comment belongs to. */
  id: number;
  resolved?: boolean;
}

/** Edit a draft review comment's text, or replace its anchor. */
export interface ReviewsCommentsUpdateInput {
  anchor?: ReviewAnchorDto | null;
  anchor_kind?: ReviewAnchorKindDto | null;
  body?: string | null;
  /** The comment to update. */
  comment_id: number;
  /** Optimistic-concurrency guard on the review's draft revision. */
  expected_revision: number;
  /** The review the comment belongs to. */
  id: number;
  /**
   * The subject version the replacement anchor was taken against.
   * Required together with `anchor_kind` and `anchor`.
   */
  subject_version?: string | null;
}

/** Create or reuse a draft review over a session's artifact or its change-set, seeding it against the currently-visible subject version. */
export interface ReviewsCreateInput {
  /** The session whose artifact or change-set is under review. */
  session: string;
  /**
   * Artifact name for `subject_kind = "artifact"`, or `"changes"` for
   * `subject_kind = "changes"`.
   */
  subject_key: string;
  subject_kind: ReviewSubjectKindDto;
  /**
   * The subject version this draft starts from: an artifact revision
   * number, or the current change-set version (which must match exactly
   * for a changes review).
   */
  subject_version: string;
}

/** Permanently discard a draft review. */
export interface ReviewsDiscardInput {
  /**
   * Optimistic-concurrency guard on the review's draft revision, as
   * `loom review ls` or the previous mutation reported it.
   */
  expected_revision: number;
  /** The draft review to discard. */
  id: number;
}

export interface ReviewsDiscardOutput {
  discarded: boolean;
}

/** Fetch a durable review by id, refreshed against its subject's current version. */
export interface ReviewsGetInput {
  /** The review to fetch. */
  id: number;
}

/** List a session's reviews for one subject — an artifact or its change-set. */
export interface ReviewsListInput {
  /**
   * The artifact name for `subject_kind = "artifact"`, or `"changes"` for
   * `subject_kind = "changes"`.
   */
  subject_key: string;
  subject_kind: ReviewSubjectKindDto;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Retarget a draft review's subject onto its current version — an artifact's latest revision, or the branch's current change-set — in one step, without touching anything else. */
export interface ReviewsRetargetInput {
  /**
   * Optimistic-concurrency guard on the review's draft revision, as
   * `loom review show` or the previous mutation reported it.
   */
  expected_revision: number;
  /** The draft review to retarget. */
  id: number;
}

/** Retry a submitted review's delivery after it failed. */
export interface ReviewsRetryDeliveryInput {
  /** The submitted review whose delivery failed. */
  id: number;
}

/** Submit a review's draft, delivering its structured feedback into the reviewed session's own conversation. */
export interface ReviewsSubmitInput {
  /**
   * Acknowledge that the review's subject moved since it was drafted, and
   * submit against the newer version anyway.
   */
  acknowledge_outdated?: boolean;
  /** Optimistic-concurrency guard on the review's draft revision. */
  expected_revision: number;
  /** The review to submit. */
  id: number;
}

/** Edit a draft review's summary, or retarget it onto a caller-supplied subject version. */
export interface ReviewsUpdateInput {
  /** Optimistic-concurrency guard on the review's draft revision. */
  expected_revision: number;
  /** The draft review to update. */
  id: number;
  /**
   * A newer subject version to retarget onto: an artifact revision number
   * for an artifact review, or the current change-set version for a
   * changes review (which must match the current version exactly).
   */
  subject_version?: string | null;
  summary?: string | null;
}

/** Reserve and launch (or idempotently return) one automation-triggered session: the entry point GitHub Actions, ops scripts, and Grafana alerts call through their federated automation credential. */
export interface RunsCreateInput {
  /**
   * Stable conversation route for related deliveries. Each idempotency key
   * remains a distinct run; channel deliveries reuse one live ACP session.
   */
  channel?: string | null;
  /**
   * Caller-selected durable key. Verified GitHub callers may leave this
   * blank to use repository/run/attempt, or provide a bounded deterministic
   * key that Loom namespaces to the verified identity.
   */
  idempotency_key?: string;
  /** Launch profile the run executes under. */
  profile: string;
  /** The session to launch. */
  session: RunsCreateNestedInput;
  /**
   * The Slack thread this delivery was announced in, so the session it
   * lands on can reply there.
   */
  slack?: SlackThreadRef | null;
  /** Trigger source: `actions`, `ops`, or `grafana`. */
  source?: string;
  /** The originating watch, when this run was triggered by a watch program. */
  watch_id?: string | null;
}

/** Inspect one automation-triggered run by id. */
export interface RunsGetInput {
  /** The run id. */
  id: string;
}

/** List automation-triggered runs (GitHub Actions / ops / Grafana deliveries): their status, launched session, and outcome. */
export interface RunsListInput {
}

/** Clear a placement default, so newly created sessions matching this selector fall through to a broader default (or the fallback origin `*`, which cannot itself be removed). */
export interface SessionLayoutDefaultsDeleteInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /**
   * Which kind of selector the default to clear matches on: `origin`,
   * `profile`, or `watch`.
   */
  selector_kind: SessionPlacementSelectorKind;
  selector_value: string;
}

/** Set (or replace) the default group a newly created session lands in for one selector. */
export interface SessionLayoutDefaultsSetInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The group matching sessions land in. */
  group_id: string;
  /**
   * Which kind of selector this default matches on: `origin`,
   * `profile`, or `watch`.
   */
  selector_kind: SessionPlacementSelectorKind;
  selector_value: string;
}

/** Subscribe to layout changes as other dashboard tabs make them. */
export interface SessionLayoutEventsInput {
}

/** The signed-in operator's shared session-dashboard layout: spaces, groups, session placements, and per-selector placement defaults. */
export interface SessionLayoutGetInput {
}

/** Create a new group within a space. */
export interface SessionLayoutGroupsCreateInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  name: string;
  /** The space the group is created in. */
  space_id: string;
}

/** Delete a group. Deleting a group never deletes sessions: `destination_group_id` is required whenever the group owns placements or default-placement selectors, and its contents move there atomically. */
export interface SessionLayoutGroupsDeleteInput {
  /**
   * Where the group's sessions and placement defaults land. Required
   * unless the group is empty.
   */
  destination_group_id?: string | null;
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The group being deleted. */
  id: string;
}

/** Set whether one group is collapsed in the caller's own dashboard. */
export interface SessionLayoutGroupsPreferenceSetInput {
  collapsed?: boolean;
  /** The group whose disclosure state is being set. */
  id: string;
}

/** Rename a group. */
export interface SessionLayoutGroupsUpdateInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The group being renamed. */
  id: string;
  name: string;
}

/** Atomically move one or more sessions to an exact insertion point within a group. */
export interface SessionLayoutMoveInput {
  /**
   * Insert before this session in the destination group; omitted appends
   * to the end.
   */
  before_session_id?: string | null;
  /** The group they move into. */
  destination_group_id: string;
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The sessions to move, in the order they should land. */
  session_ids?: string[];
}

/** Reorder one space, or one group (optionally into another space). */
export interface SessionLayoutReorderInput {
  /** Insert before this sibling; omitted moves to the end. */
  before_id?: string | null;
  /** For a group, move it into this space; omitted keeps its current space. */
  destination_space_id?: string | null;
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The space or group being repositioned. */
  id: string;
  /** Whether `id` names a `space` or a `group`. */
  kind: SessionLayoutItemKind;
}

/** Atomically restore the complete membership and order of a set of groups. */
export interface SessionLayoutRestoreInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** A JSON array of `{"group_id":"…","session_ids":["…"]}` objects. */
  groups?: SessionGroupOrderReq[];
}

/** Create a new top-level space, seeded with an "Inbox" group. */
export interface SessionLayoutSpacesCreateInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  name: string;
}

/** Delete a space. Deleting a non-empty space atomically moves its sessions and placement defaults to `destination_group_id`, which is required unless the space is empty. The last remaining space cannot be deleted. */
export interface SessionLayoutSpacesDeleteInput {
  /**
   * Where the space's sessions and placement defaults land. Required
   * unless the space is empty.
   */
  destination_group_id?: string | null;
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The space being deleted. */
  id: string;
}

/** Rename a space. */
export interface SessionLayoutSpacesUpdateInput {
  /**
   * Optimistic-concurrency guard: the layout revision this call was
   * composed against. A stale revision is rejected; omitting it applies
   * the change to whatever is current.
   */
  expected_revision?: number | null;
  /** The space being renamed. */
  id: string;
  name: string;
}

/** Rejoin an orphaned session to the active fleet: recreate its terminal (or resume its ACP runtime) in place, without touching the worktree or branch. */
export interface SessionsAdoptInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Archive a session: tear down its terminal and worktree, keeping the branch, its commits, the session row, and run history. The inverse of `recover`. */
export interface SessionsArchiveInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The session's uncommitted worktree changes against its base branch. */
export interface SessionsChangesInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The journaled ACP conversation plus the agent-owned composer metadata, paged newest-first. */
export interface SessionsChatInput {
  /** Page before this sequence number within `before_turn`. */
  before_seq?: number | null;
  /** Page before this turn (paired with `before_seq`). */
  before_turn?: number | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Subscribe to an ACP session's assistant token deltas. */
export interface SessionsChatStreamInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Change one agent-owned session configuration selector. Waits for the adapter's response and returns its full refreshed option list (also broadcast to chat clients as a `metadata` event). */
export interface SessionsConfigSetInput {
  /** Which configuration selector to change. */
  config_id: string;
  /** The new value for this option. */
  value: unknown;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Resolve this caller's session, branch, repository, channel, and links. */
export interface SessionsContextInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The session's agent conversation as a normalized iris log — the live transcript when present, else the capture archived alongside it. Oversized tool payloads are elided to a preview naming `sessions.conversation.block` and the coordinates that fetch the rest. */
export interface SessionsConversationInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Fetch a full conversation block that was elided in the main conversation view, addressed by message and block position. */
export interface SessionsConversationBlockInput {
  /** Which block within that message. */
  block: number;
  /** Which message in the conversation. */
  message: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Fully remove a session: tear down its terminal/worktree and, unless `keep_branch` is set, the branch and its commits too. The session row and run history are removed as well. This is irreversible; see `sessions.archive` to keep session data. */
export interface SessionsDeleteInput {
  /**
   * Keep the branch (and its commits) instead of deleting it along with
   * the session.
   */
  keep_branch?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Record a trusted agent lifecycle event. */
export interface SessionsEventsCreateInput {
  /** Arbitrary event payload. */
  data?: unknown;
  /** The event kind, e.g. an agent hook name. */
  kind: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** List recent durable session events. */
export interface SessionsEventsListInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Subscribe to one session's live event feed. */
export interface SessionsEventsStreamInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Worktree file completion for the chat composer: tracked plus unignored untracked paths, optionally filtered by a case-insensitive substring. */
export interface SessionsFilesInput {
  /** Case-insensitive substring filter. Blank matches everything. */
  q?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Inspect one session and its branch projection. */
export interface SessionsGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** List the repository access a session has been granted. */
export interface SessionsGithubAccessListInput {
  /** A visible session id. */
  session: string;
}

/** Clear an explicit PR mapping and return to automatic current-open-PR discovery. */
export interface SessionsGithubClearInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Add labels to the pull request currently associated with a session. */
export interface SessionsGithubLabelsAddInput {
  /** 1 to 10 label names to add to the pull request. */
  labels?: string[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Re-fetch the pull request currently associated with a session (by explicit mapping, or by automatic current-open-PR discovery) and refresh its cached status. */
export interface SessionsGithubRefreshInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Pin a session's branch to an explicit pull request and fetch it immediately. The mapping is persisted only after GitHub confirms the number, so a typo never replaces a working association with a dead one. */
export interface SessionsGithubSetInput {
  /** The pull request number to pin to. */
  pr_number: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Replace the provider behind an idle ACP session while preserving Loom's stable session/branch/worktree identity and canonical journal. */
export interface SessionsHandoffInput {
  /** Runtime selector (deprecated; use `selection` instead). */
  agent?: string;
  /** Blank/absent uses the target runtime's default. */
  effort?: string | null;
  /** Optimistic-concurrency guard against the previewed profile. */
  expected_profile_revision?: number | null;
  /** Optimistic-concurrency guard against the previewed resolver snapshot. */
  expected_resolver_revision?: string | null;
  /** ACP permission posture. Blank/absent uses the configured `agent.mode`. */
  mode?: string | null;
  /** Blank/absent uses the target runtime's default. */
  model?: string | null;
  /** The resolved profile and per-launch overrides, previewed beforehand. */
  selection?: LaunchSelection | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Preview a handoff without applying it: resolve a selection to the exact non-secret template that would be applied to the session. */
export interface SessionsHandoffResolveInput {
  /** The profile and per-launch overrides to resolve. */
  selection: LaunchSelection;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Page this session's normalized records, newest tail first; follow `older_cursor` backward for the page before. */
export interface SessionsHistoryListInput {
  /** Page backward from this cursor (exclusive). Omit for the newest tail. */
  before?: string | null;
  /** Restrict to these record kinds. */
  kinds?: HistoryKind[];
  /** Maximum records to return. */
  limit?: number | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Search this session's normalized records for literal text, case insensitively, paging the same way `sessions.history.list` does. */
export interface SessionsHistorySearchInput {
  /** Page backward from this cursor (exclusive). Omit for the newest tail. */
  before?: string | null;
  /** Restrict to these record kinds. */
  kinds?: HistoryKind[];
  /** Maximum records to return. */
  limit?: number | null;
  /** Case-insensitive literal search text. */
  q: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Whether the embedded editor (code-server) is enabled and runnable on this host, so a client can decide whether to offer it. */
export interface SessionsIdeInfoInput {
}

/** Interrupt a session's active turn. */
export interface SessionsInterruptInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Launch a child session from a task or claimed work item. */
export interface SessionsLaunchInput {
  /** Agent runtime to launch; blank uses the profile's default. */
  agent?: string | null;
  /** Base branch or ref to fork from. */
  base?: string | null;
  /** A pre-existing Loom backlog item to claim for this session. */
  claim_issue?: number | null;
  /**
   * Session class override: `"interactive"` or `"automation"` (anything
   * else is rejected). Blank/absent derives from the launch origin
   * (watch/actions/ops/grafana → automation, else interactive).
   */
  class?: string | null;
  /**
   * Local worktree path to fork the session's worktree from, when not
   * launching against a managed `repo`.
   */
  cwd?: string;
  /** Reasoning-effort override. */
  effort?: string | null;
  /** Attach to a branch that already exists rather than creating one. */
  existing_branch?: string | null;
  /**
   * Optimistic-concurrency guards: the profile and resolver revisions the
   * caller previewed against. A launch whose configuration changed underneath
   * it is rejected rather than silently run with different settings.
   */
  expected_profile_revision?: number | null;
  /** The resolver revision is a content hash, not a counter. */
  expected_resolver_revision?: string | null;
  /** A GitHub issue number to link the session to. */
  github_issue?: number | null;
  /** Detailed goal for the new session; defaults to the task label. */
  goal?: string | null;
  /** An existing GitHub issue number to seed the session from. */
  issue?: number | null;
  /**
   * The ACP launch permission posture (`auto` | `bypassPermissions` |
   * `acceptEdits` | `default` | `plan`). Blank/absent uses the configured
   * `agent.mode` (which defaults to `auto`). Ignored for a terminal launch.
   */
  mode?: string | null;
  /** Model override, when the profile's default is not wanted. */
  model?: string | null;
  /** Explicit branch name instead of a generated one. */
  name?: string | null;
  /** Named launch profile; blank selects `default`. */
  profile?: string | null;
  /**
   * Execution-backend override: `"terminal"` forces the PTY fallback for a
   * builtin; `"acp"` opts in explicitly. Blank/absent uses the agent's
   * declared default (acp for the builtins). Rejected for agents that don't
   * support the requested backend.
   */
  protocol?: string | null;
  /** A managed repository (GitHub `owner/name`) to launch against. */
  repo?: string | null;
  /** Files to seed the session's scratch directory with. */
  scratch?: ScratchUpload[];
  /**
   * The resolved profile and per-launch overrides.
   *
   * Carries the agent, model, effort, and MCP access the caller previewed.
   */
  selection?: LaunchSelection | null;
  /**
   * One-line task label for the new session.
   *
   * Optional: derived from a claimed issue or managed repo branch name if omitted.
   */
  title?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  parent_branch?: string;
}

/** Resolve a launch selection to its exact non-secret template snapshot — agent, model, effort, protocol, mode, capacity, and provenance — without launching a session. `loom sessions launch` runs this as a canonical preflight; not exposed as its own CLI verb since callers reach it through that preview instead. */
export interface SessionsLaunchesResolveInput {
  /** The profile and per-launch overrides to resolve. */
  selection: LaunchSelection;
}

/** List and search visible sessions. */
export interface SessionsListInput {
  /** Return only archived sessions (the History view). */
  archived_only?: boolean;
  /** Filter by attention level. */
  attention?: SessionSearchAttention | null;
  /**
   * Include automation-class sessions.
   *
   * Defaults to including them, which is what a fleet listing means by
   * "every session". `loom ps` passes `false` for an interactive-only inventory.
   */
  automation?: boolean;
  /** Filter by who created the session, relative to the caller. */
  creator?: SessionCreatorFilter | null;
  /** Widen the search to include recently archived sessions. */
  history?: boolean;
  /**
   * Include engine-managed warm sessions.
   *
   * An operator inventory escape hatch, refused to anything but a human
   * credential: normal fleet and survey callers must not see a watcher's own
   * infrastructure, because a watch that can see its own warm session can
   * recurse into it.
   */
  managed?: boolean;
  /** Case-insensitive search over title, goal, branch, and tags. */
  q?: string;
  /** Filter by lifecycle status. */
  status?: SessionSearchStatus | null;
}

/** Change an ACP session's permission mode (`session/set_mode`), journaling a `mode_change` block. */
export interface SessionsModeInput {
  /** Who is changing it (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The mode id to switch to, as advertised by the adapter's metadata. */
  mode_id: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Answer a pending in-flight ACP permission prompt by its chosen option: 404 for an unknown request id, 409 when it was already resolved. */
export interface SessionsPermissionsAnswerInput {
  /** Who is answering (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The chosen option's id, as advertised by the prompt. */
  option_id: string;
  /** The live permission request to answer. */
  request_id: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Read a bounded terminal preview. */
export interface SessionsPreviewInput {
  /**
   * Extra scrollback lines to include above the visible screen (0 = just
   * the visible pane).
   */
  lines?: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Send a user message to an ACP session. Dispatched immediately when idle, or appended to the durable queue while a turn is live; `send_now` instead cancels any live turn and starts the message as a normal prompt. Every send records a `nudge` event on the branch (the audit rule). */
export interface SessionsPromptCreateInput {
  /** Worktree-relative files to attach as ACP resource links. */
  files?: string[];
  /**
   * Promote the server's durable next-turn queue instead of sending
   * `text`. Keeps the action race-free when a client is showing queued
   * copy.
   */
  force_queued?: boolean;
  /** Cancel any live turn and start this message as a normal prompt. */
  send_now?: boolean;
  /** The message text. */
  text: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Pull unseen next-turn feedback back out of the durable queue for editing. The ACP task owns the consume so this action is serialized with automatic dispatch at a turn boundary. */
export interface SessionsPromptRetractInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Raw bytes of a worktree file, with a guessed content type — for inline image previews and downloads. Always reads the working tree, never a git ref. */
export interface SessionsRawInput {
  /** Worktree-relative path to read. */
  path?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Recover an archived session: rebuild its worktree from the kept branch, then resume the agent. For a live (non-archived) session, restart its ACP runtime instead. The inverse of `archive`. */
export interface SessionsRecoverInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Generate the session's resumption cue if it is missing or stale. `force` regenerates it unconditionally; otherwise the configured inactivity threshold applies, as on the on-return path. */
export interface SessionsResumptionCueEnsureInput {
  /**
   * Regenerate unconditionally instead of respecting the inactivity
   * threshold.
   */
  force?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The session's current resumption cue, if one has been generated. */
export interface SessionsResumptionCueGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Delete one Scratch file. */
export interface SessionsScratchDeleteInput {
  /** The file name to delete. */
  name: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Shared upload limits for launch-time and live-session Scratch attachments. */
export interface SessionsScratchLimitsInput {
}

/** List a session's Scratch files. */
export interface SessionsScratchListInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Write one Scratch file from a raw request body. */
export interface SessionsScratchWriteInput {
  /** The file name to write, a single path component. */
  name?: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Deliver a new prompt to a session. */
export interface SessionsSendInput {
  /** Who is sending (a watch name, or blank for `manual`). */
  by?: string | null;
  /**
   * Whether to follow the text with Enter to submit it as a turn. Omit for
   * the default (submit); pass `false` to stage input unsubmitted.
   */
  submit?: boolean | null;
  /** The text to type into the agent's pane. */
  text: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Close one of a session's worktree debug shells, killing its supervisor. */
export interface SessionsShellsDeleteInput {
  /** Which of the session's debug shells to close. */
  index: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The live worktree debug-shell indices for a session, so a client re-opens the shell tabs after a reload. Never spawns. */
export interface SessionsShellsListInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Attach to one of a session's worktree debug shells over a websocket. */
export interface SessionsShellsTerminalInput {
  /** Which of the session's debug shells; several may run at once. */
  index?: number;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Read the session's durable attention level and status message. */
export interface SessionsStatusGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Update the durable attention level and status message. */
export interface SessionsStatusSetInput {
  /** The attention level. */
  level: 'ok' | 'attention' | 'blocked';
  /** The current-state message shown alongside the level. */
  message?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Return the current goal, status, inbox, artifacts, issues, and next actions. */
export interface SessionsSummaryGetInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The fleet index: one compact row per visible session. */
export interface SessionsSummaryListInput {
  /** Include archived rows alongside active work. */
  archived?: boolean;
  /** Return only archived rows. Implies `archived`. */
  archived_only?: boolean;
  /** Filter by attention level. */
  attention?: SessionSearchAttention | null;
  /** Include automation-class sessions. */
  automation?: boolean;
  /** Filter by who created the session, relative to the caller. */
  creator?: SessionCreatorFilter | null;
  /** Case-insensitive search over the same facets as fleet search. */
  q?: string;
  /** Filter by lifecycle status. */
  status?: SessionSearchStatus | null;
}

/** Remove one free-form session tag. */
export interface SessionsTagsDeleteInput {
  /** Who is clearing it (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The tag key to remove. */
  key: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** List free-form tags on a session. */
export interface SessionsTagsListInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Atomically replace one author's complete tag set on a session. */
export interface SessionsTagsReplaceInput {
  /** The author whose existing tag set is replaced. Defaults to `manual`. */
  by?: string | null;
  /** Exact `(key, value)` pairs to clear in the same transaction. */
  clear?: TagMatch[];
  /** The complete tag set this author now asserts. */
  tags?: TagInput[];
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Set one free-form session tag. */
export interface SessionsTagsSetInput {
  /** Who is setting it (a watch name, or blank for `manual`). */
  by?: string | null;
  /** The tag key. */
  key: string;
  /** One-line reason accompanying the tag. */
  note?: string;
  /** The tag value. */
  value: string;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Attach to a session's agent terminal over a websocket. */
export interface SessionsTerminalInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Toggle whether Loom generates this session's title automatically. */
export interface SessionsTitleGenerationSetInput {
  /** Whether automatic title generation is enabled. */
  enabled?: boolean;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Regenerate a session's title immediately, bypassing the confidence guard that normally throttles automatic generation. */
export interface SessionsTitleRegenerateInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Update a session's branch-level fields (title, goal, description) and its durable status. Attention level is managed via tags operations (`sessions.tags.set`/`sessions.tags.delete`). */
export interface SessionsUpdateInput {
  /**
   * The agent's current-state message — the prose shown beside the
   * attention level.
   */
  description?: string | null;
  /**
   * Required with `title`: the label the caller last observed. Used to detect
   * and reject concurrent updates by comparing with the current value.
   */
  expected_title?: string | null;
  /**
   * Required with `title`: the provenance (`user` or `agent`) the caller
   * last observed.
   */
  expected_title_provenance?: string | null;
  /** New goal text for the branch. */
  goal?: string | null;
  /** New durable status (the fleet lifecycle marker). */
  status?: string | null;
  /** New task label for the branch. */
  title?: string | null;
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** The externally-visible dashboard URL for a session. The agent inside a session only knows its loopback API address, so only the server can resolve this — from the configured `auth.base_url`, or the address it is bound to. */
export interface SessionsUrlInput {
  /** Supplied by the dispatcher from the caller's session context when omitted. */
  session?: string;
}

/** Remove one variable from the default profile's environment. A missing name is not an error — the desired end state already holds. */
export interface SettingsEnvDeleteInput {
  /** The variable name. */
  name: string;
}

/** List every variable in the default profile's environment. Unlike a named profile's environment metadata, values are returned in full. */
export interface SettingsEnvListInput {
}

/** Upsert one variable in the default profile's environment. The value is free-form; the name is validated as a shell identifier so it cannot corrupt the launch script that exports it. */
export interface SettingsEnvSetInput {
  /** The variable name — a POSIX-portable shell identifier. */
  name: string;
  /** The value to store. */
  value: string;
}

/** Every registered runtime setting and its effective value. */
export interface SettingsGetInput {
}

/** Apply setting changes. A `null` value clears a key back to its default. */
export interface SettingsPatchInput {
  /**
   * Dotted setting key to new value; `null` clears that key back to its
   * default.
   *
   * A value may be a string, a boolean, or a number. Settings are *stored* as
   * strings, but a caller naturally writes the setting's own type — `false`
   * for `auth.trust_loopback`, `300` for a `_secs` key. Coercion happens once,
   * server-side; anything else (an array, an object) is rejected by key.
   */
  changes?: Record<string, unknown>;
}

/** Restart the standalone operator shell, discarding its process state. */
export interface ShellRestartInput {
}

/** Attach to the standalone operator shell over a websocket. */
export interface ShellTerminalInput {
}

/** Slack integration status: which credentials are set, whether `auth.test` resolves a live bot identity, the configured access boundary, and the Socket Mode supervisor's live health. */
export interface SlackConnectionStatusInput {
}

export interface SlackConnectionStatusOutput {
  access: SlackAccessView;
  app_token_set: boolean;
  bot_token_set: boolean;
  configured: boolean;
  default_repo: string;
  enabled: boolean;
  /** `None` when no bot credential is configured at all. */
  identity: SlackIdentityView | null;
  socket: SlackSocketView;
}

/** List recent detached background tasks — currently the GitHub `@loom` trigger launches, which run off the webhook request so a slow clone can't blow GitHub's delivery timeout — newest first. */
export interface TasksListInput {
}

/** Register a watch. */
export interface WatchesCreateInput {
  /**
   * The granted capability set (the intervention ladder). `observe` is
   * implicit; the rest are explicit grants.
   */
  capabilities?: string[] | null;
  cooldown_secs?: number | null;
  effort?: string | null;
  /**
   * Whether the watch fires as soon as it is created. Omitted clients get
   * the model default (disabled); the loom UI sends `true` so a watcher
   * picked from the builtin registry is live without a separate manual
   * enable.
   */
  enabled?: boolean | null;
  model?: string | null;
  /** The watch's unique name. */
  name: string;
  /** Stock-program parameters (e.g. the judgement `prompt`). */
  params?: unknown;
  /**
   * Automation-safe ACP launch profile used for agent judgements and warm
   * sessions.
   */
  profile?: string | null;
  /**
   * `builtin:<name>` for a stock program, or an absolute path under
   * `~/.weaver/watches/` for a custom one.
   */
  program?: string | null;
  /** The fleet query a round surveys: `{attention?, repo?}`. */
  scope?: unknown;
  /**
   * The event-match predicate: `{cron|every|event|level|repo}`. Defaults to
   * the program's declared trigger (register-mode manifest), or an empty
   * predicate.
   */
  trigger?: unknown;
}

/** Remove a watch. */
export interface WatchesDeleteInput {
  /** Watch id or name. */
  key: string;
}

/** Inspect one watch by id or name. */
export interface WatchesGetInput {
  /** Watch id or name. */
  key: string;
}

/** List every registered watch: name, enabled, trigger, program, last outcome. */
export interface WatchesListInput {
}

/** List the builtin watch programs that ship with loom. */
export interface WatchesProgramsInput {
}

/** Fire a watch round now, in the daemon, and report its outcome. `dry_run` stubs every mutating action — the iteration primitive, safe to repeat. */
export interface WatchesRunInput {
  /**
   * Simulate: every mutating action is stubbed and logged as "would do X",
   * nothing is performed.
   */
  dry_run?: boolean;
  /** Watch id or name. */
  key: string;
}

/** Show a watch's round history: time, trigger reason, outcome, summary, and the captured stdout/stderr/exit status of each round. */
export interface WatchesRunsInput {
  /** Watch id or name. */
  key: string;
  /** How many recent rounds to return; defaults to 50, clamped to 1000. */
  limit?: number | null;
}

/** Update a watch's settings, optionally arm or disarm it via the `enabled` field. */
export interface WatchesUpdateInput {
  /** The granted capability set (the intervention ladder). */
  capabilities?: string[] | null;
  cooldown_secs?: number | null;
  effort?: string | null;
  /** Arm (`true`) or disarm (`false`) the watch. */
  enabled?: boolean | null;
  /** Watch id or name. */
  key: string;
  model?: string | null;
  /** Stock-program parameters (e.g. the judgement `prompt`). */
  params?: unknown;
  /** Automation-safe ACP launch profile. */
  profile?: string | null;
  /**
   * `builtin:<name>` for a stock program, or an absolute path under
   * `~/.weaver/watches/` for a custom one.
   */
  program?: string | null;
  /** The fleet query a round surveys: `{attention?, repo?}`. */
  scope?: unknown;
  /**
   * The event-match predicate: `{cron|every|event|level|repo}`. Setting the
   * program without an explicit trigger re-evaluates the new program's
   * register-mode manifest.
   */
  trigger?: unknown;
}

// -- The operation table --------------------------------------------------

/** Every registered operation, keyed by its identity. An id this map does not
 * carry is a compile error, not a 404. */
export interface Operations {
  /** Define a new custom agent — a name, a label, and a shell command per launch stage — so it appears in the picker beside the builtin `claude`/`codex` without a code change. */
  'agents.custom.create': { input: AgentsCustomCreateInput; output: CustomAgentsView };
  /** Remove a custom agent. Removing an absent name is a no-op. Sessions already launched with it are unaffected. */
  'agents.custom.delete': { input: AgentsCustomDeleteInput; output: CustomAgentsView };
  /** Replace an existing custom agent's definition. The name is immutable; a builtin or unknown name is rejected. */
  'agents.custom.update': { input: AgentsCustomUpdateInput; output: CustomAgentsView };
  /** List available agent runtimes: builtins, operator-defined custom agents, and the configured default. */
  'agents.list': { input: AgentsListInput; output: AgentsView };
  /** Run a one-shot ACP prompt through a registered agent runtime and return its text — the judgement-call primitive watch programs call. */
  'agents.oneshot': { input: AgentsOneshotInput; output: AgentsOneshotOutput };
  /** Delete an artifact and its complete revision history. */
  'artifacts.delete': { input: ArtifactsDeleteInput; output: ArtifactDeleteResult };
  /** Read one artifact or immutable revision. */
  'artifacts.get': { input: ArtifactsGetInput; output: ArtifactView };
  /** List immutable artifact revisions. */
  'artifacts.history': { input: ArtifactsHistoryInput; output: ArtifactVersion[] };
  /** List branch and repository-scoped artifacts. */
  'artifacts.list': { input: ArtifactsListInput; output: ArtifactMeta[] };
  /** An image artifact's decoded bytes, for an `<img src>`. */
  'artifacts.raw': { input: ArtifactsRawInput; output: null };
  /** Start or reply to an artifact review thread. */
  'artifacts.threads.comment': { input: ArtifactsThreadsCommentInput; output: ThreadDto };
  /** List anchored artifact review threads. */
  'artifacts.threads.list': { input: ArtifactsThreadsListInput; output: ThreadDto[] };
  /** Resolve an artifact review thread. */
  'artifacts.threads.resolve': { input: ArtifactsThreadsResolveInput; output: ThreadDto };
  /** The externally-visible dashboard deep-link for an artifact. */
  'artifacts.url': { input: ArtifactsUrlInput; output: SessionUrlView };
  /** Create an artifact or append a guarded revision. */
  'artifacts.write': { input: ArtifactsWriteInput; output: ArtifactView };
  /** Mint a short-lived automation-only token for a given subject. */
  'auth.automation_token': { input: AuthAutomationTokenInput; output: AutomationTokenView };
  /** Exchange a workload-identity OIDC token for a short-lived automation token, per a mapping an admin registered with `auth.federations.create`. */
  'auth.federate': { input: AuthFederateInput; output: AutomationTokenView };
  /** Register (or idempotently reconcile) a workload-identity federation mapping — the trust relationship `auth.federate` exchanges an OIDC token against. */
  'auth.federations.create': { input: AuthFederationsCreateInput; output: FederationView };
  /** List the registered workload-identity federation mappings. */
  'auth.federations.list': { input: AuthFederationsListInput; output: FederationView[] };
  /** Remove a workload-identity federation mapping. */
  'auth.federations.remove': { input: AuthFederationsRemoveInput; output: RemoveFederationResult };
  /** Read the GitHub sign-in / App setup (secret withheld). */
  'auth.github_config.get': { input: AuthGithubConfigGetInput; output: GithubConfigView };
  /** Set the GitHub sign-in OAuth client id (and, optionally, its secret). */
  'auth.github_config.set': { input: AuthGithubConfigSetInput; output: GithubConfigView };
  /** Whether the caller has a personal GitHub token on file, and when it last changed. */
  'auth.github_token.get': { input: AuthGithubTokenGetInput; output: GithubTokenStatusView };
  /** Remove the caller's personal GitHub token. */
  'auth.github_token.remove': { input: AuthGithubTokenRemoveInput; output: GithubTokenStatusView };
  /** Set the caller's personal GitHub token. Loom selects it for ordinary interactive sessions this user launches; restricted sessions never use it. */
  'auth.github_token.set': { input: AuthGithubTokenSetInput; output: GithubTokenStatusView };
  /** Exchange a username and password for a signed-in session. */
  'auth.login': { input: AuthLoginInput; output: MeView };
  /** End the caller's signed-in session. */
  'auth.logout': { input: AuthLogoutInput; output: MeView };
  /** Who the caller is, and which sign-in methods the server offers. */
  'auth.me': { input: AuthMeInput; output: MeView };
  /** Set or change the caller's own password. */
  'auth.set_password': { input: AuthSetPasswordInput; output: UserView };
  /** Mint a new personal API token. The plaintext is returned once — the server keeps only a hash. */
  'auth.tokens.create': { input: AuthTokensCreateInput; output: CreatedTokenView };
  /** List the caller's own personal API tokens (metadata only; secrets are never returned). */
  'auth.tokens.list': { input: AuthTokensListInput; output: TokenView[] };
  /** Revoke one of the caller's own personal API tokens. */
  'auth.tokens.revoke': { input: AuthTokensRevokeInput; output: RevokeTokenResult };
  /** Add a new operator to the approved allowlist. */
  'auth.users.create': { input: AuthUsersCreateInput; output: UserView };
  /** List the approved operators. */
  'auth.users.list': { input: AuthUsersListInput; output: UserView[] };
  /** Remove an approved operator. A caller may not remove themself. */
  'auth.users.remove': { input: AuthUsersRemoveInput; output: RemoveUserResult };
  /** Change an operator's role. Existing cookies and personal tokens observe the change on their next request. */
  'auth.users.set_role': { input: AuthUsersSetRoleInput; output: UserView };
  /** Append a raw event row to a branch's log — the escape hatch for an event kind with no dedicated mutating route of its own. */
  'branches.events.create': { input: BranchesEventsCreateInput; output: Event };
  /** List recent durable events on a branch (newest first, last 200 entries). */
  'branches.events.list': { input: BranchesEventsListInput; output: Event[] };
  /** Inspect one branch. */
  'branches.get': { input: BranchesGetInput; output: BranchView };
  /** List work items claimed by this branch — the session's working set. */
  'branches.issues.list': { input: BranchesIssuesListInput; output: IssueView[] };
  /** List every branch loom is tracking (fleet-wide, unfiltered). */
  'branches.list': { input: BranchesListInput; output: BranchView[] };
  /** Post a message from this branch's session back to a Slack thread. */
  'branches.slack.reply': { input: BranchesSlackReplyInput; output: unknown };
  /** Set the branch's attention level and current-state message in one call. */
  'branches.status.set': { input: BranchesStatusSetInput; output: BranchView };
  /** Remove one free-form tag from a branch — the branch-scoped twin of `sessions.tags.delete`. */
  'branches.tags.delete': { input: BranchesTagsDeleteInput; output: BranchView };
  /** Set one free-form tag on a branch — the branch-scoped twin of `sessions.tags.set`, for a target with no live session bound to it (a finished session, or an id naming another branch entirely). */
  'branches.tags.set': { input: BranchesTagsSetInput; output: BranchView };
  /** Update a branch's title, goal, or current-state description. */
  'branches.update': { input: BranchesUpdateInput; output: BranchView };
  /** Archive a custom channel. */
  'channels.archive': { input: ChannelsArchiveInput; output: ChannelArchiveResult };
  /** List a channel's external delivery bindings: subscribed session inboxes, plus the originating Slack thread if the branch is wired to one. */
  'channels.bindings.list': { input: ChannelsBindingsListInput; output: ChannelBindingView[] };
  /** Open a custom durable channel. */
  'channels.create': { input: ChannelsCreateInput; output: ChannelView };
  /** Inspect one channel and its delivery bindings. */
  'channels.get': { input: ChannelsGetInput; output: ChannelView };
  /** List visible durable channels and their unread state. */
  'channels.list': { input: ChannelsListInput; output: ChannelView[] };
  /** Append and deliver a durable channel message. */
  'channels.messages.create': { input: ChannelsMessagesCreateInput; output: ChannelMessageView };
  /** Read a channel's message history, advancing the read marker unless peeking. */
  'channels.messages.list': { input: ChannelsMessagesListInput; output: ChannelMessageView[] };
  /** Acknowledge a channel through a sequence number. */
  'channels.read_marker.set': { input: ChannelsReadMarkerSetInput; output: ChannelSubscriptionView };
  /** Set how a session follows a channel. */
  'channels.subscription.set': { input: ChannelsSubscriptionSetInput; output: ChannelSubscriptionView };
  /** Wait for the next matching channel message. */
  'channels.wait': { input: ChannelsWaitInput; output: ChannelMessageView };
  /** Reconcile the runtime resources declared by a deployment stack: settings, launch profiles, and federation mappings. */
  'deployment.reconcile': { input: DeploymentReconcileInput; output: DeploymentView };
  /** The aggregated fleet diagnostics snapshot: session/profile capacity, automation run health, migration state, and federation mappings. */
  'diagnostics.get': { input: DiagnosticsGetInput; output: DiagnosticsView };
  /** Build and process identity for a human operator's debug panel: which version and image are running, and since when. */
  'diagnostics.status': { input: DiagnosticsStatusInput; output: DiagnosticsStatusOutput };
  /** Subscribe to one or more event topics over a single SSE connection. */
  'events.stream': { input: EventsStreamInput; output: null };
  /** Apply one action atomically to a set of work items. */
  'issues.actions': { input: IssuesActionsInput; output: IssueActionsResult };
  /** Create an unclaimed repository backlog item. */
  'issues.backlog.create': { input: IssuesBacklogCreateInput; output: IssueView };
  /** Every work item across every repository — the dashboard's board. */
  'issues.board': { input: IssuesBoardInput; output: IssueView[] };
  /** Close one or more work items atomically. */
  'issues.close': { input: IssuesCloseInput; output: IssueActionsResult };
  /** Create a work item claimed by this session's branch. */
  'issues.create': { input: IssuesCreateInput; output: IssueView };
  /** Permanently delete one or more work items atomically. */
  'issues.delete': { input: IssuesDeleteInput; output: IssueActionsResult };
  /** Inspect one work item and the status of the branch working it. */
  'issues.get': { input: IssuesGetInput; output: IssueView };
  /** List current-session and repository work items. */
  'issues.list': { input: IssuesListInput; output: IssueView[] };
  /** Reopen one or more closed work items atomically. */
  'issues.reopen': { input: IssuesReopenInput; output: IssueActionsResult };
  /** Remove one free-form tag from a work item. */
  'issues.tags.delete': { input: IssuesTagsDeleteInput; output: IssueView };
  /** Set one free-form tag on a work item. */
  'issues.tags.set': { input: IssuesTagsSetInput; output: IssueView };
  /** Edit a work item's own fields. */
  'issues.update': { input: IssuesUpdateInput; output: IssueView };
  /** A snapshot of the most recent server log lines, oldest first. */
  'logs.list': { input: LogsListInput; output: LogLineView[] };
  /** Tail the server log as it is written. */
  'logs.stream': { input: LogsStreamInput; output: null };
  /** Add an operator-authored custom MCP server. */
  'mcps.custom.create': { input: McpsCustomCreateInput; output: CustomMcpView };
  /** Permanently remove an operator-authored custom MCP server. */
  'mcps.custom.delete': { input: McpsCustomDeleteInput; output: CustomMcpDeleteResult };
  /** Show one operator-authored custom MCP server's latest definition and validation state. */
  'mcps.custom.get': { input: McpsCustomGetInput; output: CustomMcpView };
  /** List operator-authored custom MCP servers. */
  'mcps.custom.list': { input: McpsCustomListInput; output: CustomMcpView[] };
  /** Replace an operator-authored custom MCP server's definition, producing a new validated revision. */
  'mcps.custom.update': { input: McpsCustomUpdateInput; output: CustomMcpView };
  /** The trusted MCP registry: built-in adapters, versioned capability sets, and operator-authored custom servers. */
  'mcps.get': { input: McpsGetInput; output: McpRegistryView };
  /** Show this session's effective Loom operations and external repository scope. */
  'permissions.effective.get': { input: PermissionsEffectiveGetInput; output: EffectivePermissionsView };
  /** Explain one registered operation's actor, risk, and projections. */
  'permissions.explain': { input: PermissionsExplainInput; output: OperationView };
  /** Directly grant one GitHub repository to a live session, without a prior request. */
  'permissions.github.grant': { input: PermissionsGithubGrantInput; output: SessionGithubAccessView };
  /** Invoke one fixed-target GitHub operation granted by restricted session policy. */
  'permissions.github.restricted.invoke': { input: PermissionsGithubRestrictedInvokeInput; output: RestrictedGithubToolView };
  /** Revoke one explicit GitHub repository override from a live session. */
  'permissions.github.revoke': { input: PermissionsGithubRevokeInput; output: SessionGithubAccessView };
  /** Mint a refreshable repository-scoped GitHub App credential for this session. */
  'permissions.github.token': { input: PermissionsGithubTokenInput; output: GithubTokenView };
  /** Approve and apply a pending external-access request. */
  'permissions.requests.approve': { input: PermissionsRequestsApproveInput; output: PermissionRequestView };
  /** Request a human-approved GitHub write-access expansion for this session. */
  'permissions.requests.create': { input: PermissionsRequestsCreateInput; output: PermissionRequestView };
  /** Deny a pending external-access request. */
  'permissions.requests.deny': { input: PermissionsRequestsDenyInput; output: PermissionRequestView };
  /** List durable external-access requests for this session. */
  'permissions.requests.list': { input: PermissionsRequestsListInput; output: PermissionRequestView[] };
  /** Get this operator's personal UI preference overrides (terminal theme, font, font size), each layered over its effective inherited value. */
  'preferences.get': { input: PreferencesGetInput; output: UserPreferencesEnvelope };
  /** Set or clear this operator's personal UI preferences. */
  'preferences.patch': { input: PreferencesPatchInput; output: UserPreferencesEnvelope };
  /** Clone one profile's reviewed policy into a new insert-only profile, optionally composing its write-only environment in the same transaction. If the profile changed since the caller reviewed it, this returns a fresh preview instead of silently applying a stale composition. */
  'profiles.clone': { input: ProfilesCloneInput; output: ProfileView };
  /** Create a named session-launch profile. */
  'profiles.create': { input: ProfilesCreateInput; output: ProfileView };
  /** Permanently delete a named launch profile. */
  'profiles.delete': { input: ProfilesDeleteInput; output: ProfileDeleteResult };
  /** Resolve one profile's exact non-secret policy — MCP snapshot, runtime permissions, and MCP server processes — without launching a session. */
  'profiles.effective': { input: ProfilesEffectiveInput; output: EffectiveProfileView };
  /** Remove one profile's write-only environment variable. */
  'profiles.env.delete': { input: ProfilesEnvDeleteInput; output: ProfileView };
  /** Set one profile's write-only environment variable from a literal value or GCP Secret Manager reference — exactly one of the two is required. */
  'profiles.env.set': { input: ProfilesEnvSetInput; output: ProfileView };
  /** Show one named launch profile. Secret environment values are never returned. */
  'profiles.get': { input: ProfilesGetInput; output: ProfileView };
  /** List named launch profiles. Secret environment values are never returned. */
  'profiles.list': { input: ProfilesListInput; output: ProfileView[] };
  /** Replace a named session-launch profile's policy. */
  'profiles.update': { input: ProfilesUpdateInput; output: ProfileView };
  /** List the local git branches of a repo checkout, and which has a worktree. */
  'repos.branches': { input: ReposBranchesInput; output: RepoBranchView[] };
  /** Remove one per-repo environment variable. Removing an absent name is a no-op. Returns the refreshed metadata list (no values). */
  'repos.env.delete': { input: ReposEnvDeleteInput; output: RepoEnvView };
  /** Read a repo's environment variables' metadata: names and timestamps only — values are write-only and never returned. */
  'repos.env.get': { input: ReposEnvGetInput; output: RepoEnvView };
  /** Upsert one per-repo environment variable. The name is validated as a shell identifier that isn't one of loom's reserved control or GitHub credential names, so it can't corrupt or shadow the launch environment. Returns the refreshed metadata list (no values). */
  'repos.env.set': { input: ReposEnvSetInput; output: RepoEnvView };
  /** List the registered managed repos (the clone allowlist). */
  'repos.list': { input: ReposListInput; output: RepoView[] };
  /** Recently-used repositories, most recent first — the launch flow's repo picker. */
  'repos.recent': { input: ReposRecentInput; output: RecentRepoView[] };
  /** Register a repo in the managed store — add it to the clone allowlist. The clone itself is lazy (it happens on first use); this just adds an entry. */
  'repos.register': { input: ReposRegisterInput; output: RepoView };
  /** Check whether a worktree fork point resolves against a repo checkout, matching what a launch would fork from — fetching the revision from `origin` on demand if needed. Never touches local branches or the working tree. */
  'repos.revisions.validate': { input: ReposRevisionsValidateInput; output: RepoRevisionValidationView };
  /** Append an anchored feedback comment to a draft review. */
  'reviews.comments.create': { input: ReviewsCommentsCreateInput; output: ReviewDto };
  /** Remove a draft review comment. */
  'reviews.comments.delete': { input: ReviewsCommentsDeleteInput; output: ReviewDto };
  /** Mark a comment on a submitted review resolved or unresolved. */
  'reviews.comments.resolve': { input: ReviewsCommentsResolveInput; output: ReviewCommentDto };
  /** Edit a draft review comment's text, or replace its anchor. */
  'reviews.comments.update': { input: ReviewsCommentsUpdateInput; output: ReviewDto };
  /** Create or reuse a draft review over a session's artifact or its change-set, seeding it against the currently-visible subject version. */
  'reviews.create': { input: ReviewsCreateInput; output: ReviewDto };
  /** Permanently discard a draft review. */
  'reviews.discard': { input: ReviewsDiscardInput; output: ReviewsDiscardOutput };
  /** Fetch a durable review by id, refreshed against its subject's current version. */
  'reviews.get': { input: ReviewsGetInput; output: ReviewDto };
  /** List a session's reviews for one subject — an artifact or its change-set. */
  'reviews.list': { input: ReviewsListInput; output: ReviewDto[] };
  /** Retarget a draft review's subject onto its current version — an artifact's latest revision, or the branch's current change-set — in one step, without touching anything else. */
  'reviews.retarget': { input: ReviewsRetargetInput; output: ReviewDto };
  /** Retry a submitted review's delivery after it failed. */
  'reviews.retry_delivery': { input: ReviewsRetryDeliveryInput; output: ReviewDto };
  /** Submit a review's draft, delivering its structured feedback into the reviewed session's own conversation. */
  'reviews.submit': { input: ReviewsSubmitInput; output: ReviewDto };
  /** Edit a draft review's summary, or retarget it onto a caller-supplied subject version. */
  'reviews.update': { input: ReviewsUpdateInput; output: ReviewDto };
  /** Reserve and launch (or idempotently return) one automation-triggered session: the entry point GitHub Actions, ops scripts, and Grafana alerts call through their federated automation credential. */
  'runs.create': { input: RunsCreateInput; output: RunView };
  /** Inspect one automation-triggered run by id. */
  'runs.get': { input: RunsGetInput; output: RunView };
  /** List automation-triggered runs (GitHub Actions / ops / Grafana deliveries): their status, launched session, and outcome. */
  'runs.list': { input: RunsListInput; output: RunView[] };
  /** Clear a placement default, so newly created sessions matching this selector fall through to a broader default (or the fallback origin `*`, which cannot itself be removed). */
  'session_layout.defaults.delete': { input: SessionLayoutDefaultsDeleteInput; output: SessionLayoutView };
  /** Set (or replace) the default group a newly created session lands in for one selector. */
  'session_layout.defaults.set': { input: SessionLayoutDefaultsSetInput; output: SessionLayoutView };
  /** Subscribe to layout changes as other dashboard tabs make them. */
  'session_layout.events': { input: SessionLayoutEventsInput; output: null };
  /** The signed-in operator's shared session-dashboard layout: spaces, groups, session placements, and per-selector placement defaults. */
  'session_layout.get': { input: SessionLayoutGetInput; output: SessionLayoutView };
  /** Create a new group within a space. */
  'session_layout.groups.create': { input: SessionLayoutGroupsCreateInput; output: SessionLayoutView };
  /** Delete a group. Deleting a group never deletes sessions: `destination_group_id` is required whenever the group owns placements or default-placement selectors, and its contents move there atomically. */
  'session_layout.groups.delete': { input: SessionLayoutGroupsDeleteInput; output: SessionLayoutView };
  /** Set whether one group is collapsed in the caller's own dashboard. */
  'session_layout.groups.preference.set': { input: SessionLayoutGroupsPreferenceSetInput; output: SessionLayoutView };
  /** Rename a group. */
  'session_layout.groups.update': { input: SessionLayoutGroupsUpdateInput; output: SessionLayoutView };
  /** Atomically move one or more sessions to an exact insertion point within a group. */
  'session_layout.move': { input: SessionLayoutMoveInput; output: SessionLayoutView };
  /** Reorder one space, or one group (optionally into another space). */
  'session_layout.reorder': { input: SessionLayoutReorderInput; output: SessionLayoutView };
  /** Atomically restore the complete membership and order of a set of groups. */
  'session_layout.restore': { input: SessionLayoutRestoreInput; output: SessionLayoutView };
  /** Create a new top-level space, seeded with an "Inbox" group. */
  'session_layout.spaces.create': { input: SessionLayoutSpacesCreateInput; output: SessionLayoutView };
  /** Delete a space. Deleting a non-empty space atomically moves its sessions and placement defaults to `destination_group_id`, which is required unless the space is empty. The last remaining space cannot be deleted. */
  'session_layout.spaces.delete': { input: SessionLayoutSpacesDeleteInput; output: SessionLayoutView };
  /** Rename a space. */
  'session_layout.spaces.update': { input: SessionLayoutSpacesUpdateInput; output: SessionLayoutView };
  /** Rejoin an orphaned session to the active fleet: recreate its terminal (or resume its ACP runtime) in place, without touching the worktree or branch. */
  'sessions.adopt': { input: SessionsAdoptInput; output: SessionView };
  /** Archive a session: tear down its terminal and worktree, keeping the branch, its commits, the session row, and run history. The inverse of `recover`. */
  'sessions.archive': { input: SessionsArchiveInput; output: SessionArchiveResult };
  /** The session's uncommitted worktree changes against its base branch. */
  'sessions.changes': { input: SessionsChangesInput; output: ChangeSetDto };
  /** The journaled ACP conversation plus the agent-owned composer metadata, paged newest-first. */
  'sessions.chat': { input: SessionsChatInput; output: SessionChatView };
  /** Subscribe to an ACP session's assistant token deltas. */
  'sessions.chat.stream': { input: SessionsChatStreamInput; output: null };
  /** Change one agent-owned session configuration selector. Waits for the adapter's response and returns its full refreshed option list (also broadcast to chat clients as a `metadata` event). */
  'sessions.config.set': { input: SessionsConfigSetInput; output: ConfigOptionResult };
  /** Resolve this caller's session, branch, repository, channel, and links. */
  'sessions.context': { input: SessionsContextInput; output: SelfContextView };
  /** The session's agent conversation as a normalized iris log — the live transcript when present, else the capture archived alongside it. Oversized tool payloads are elided to a preview naming `sessions.conversation.block` and the coordinates that fetch the rest. */
  'sessions.conversation': { input: SessionsConversationInput; output: Log };
  /** Fetch a full conversation block that was elided in the main conversation view, addressed by message and block position. */
  'sessions.conversation.block': { input: SessionsConversationBlockInput; output: Block };
  /** Fully remove a session: tear down its terminal/worktree and, unless `keep_branch` is set, the branch and its commits too. The session row and run history are removed as well. This is irreversible; see `sessions.archive` to keep session data. */
  'sessions.delete': { input: SessionsDeleteInput; output: DeleteResult };
  /** Record a trusted agent lifecycle event. */
  'sessions.events.create': { input: SessionsEventsCreateInput; output: Event };
  /** List recent durable session events. */
  'sessions.events.list': { input: SessionsEventsListInput; output: Event[] };
  /** Subscribe to one session's live event feed. */
  'sessions.events.stream': { input: SessionsEventsStreamInput; output: null };
  /** Worktree file completion for the chat composer: tracked plus unignored untracked paths, optionally filtered by a case-insensitive substring. */
  'sessions.files': { input: SessionsFilesInput; output: SessionFilesView };
  /** Inspect one session and its branch projection. */
  'sessions.get': { input: SessionsGetInput; output: SessionView };
  /** List the repository access a session has been granted. */
  'sessions.github.access.list': { input: SessionsGithubAccessListInput; output: SessionGithubAccessView[] };
  /** Clear an explicit PR mapping and return to automatic current-open-PR discovery. */
  'sessions.github.clear': { input: SessionsGithubClearInput; output: SessionView };
  /** Add labels to the pull request currently associated with a session. */
  'sessions.github.labels.add': { input: SessionsGithubLabelsAddInput; output: AddLabelsResult };
  /** Re-fetch the pull request currently associated with a session (by explicit mapping, or by automatic current-open-PR discovery) and refresh its cached status. */
  'sessions.github.refresh': { input: SessionsGithubRefreshInput; output: SessionView };
  /** Pin a session's branch to an explicit pull request and fetch it immediately. The mapping is persisted only after GitHub confirms the number, so a typo never replaces a working association with a dead one. */
  'sessions.github.set': { input: SessionsGithubSetInput; output: SessionView };
  /** Replace the provider behind an idle ACP session while preserving Loom's stable session/branch/worktree identity and canonical journal. */
  'sessions.handoff': { input: SessionsHandoffInput; output: SessionView };
  /** Preview a handoff without applying it: resolve a selection to the exact non-secret template that would be applied to the session. */
  'sessions.handoff.resolve': { input: SessionsHandoffResolveInput; output: ResolvedLaunchView };
  /** Page this session's normalized records, newest tail first; follow `older_cursor` backward for the page before. */
  'sessions.history.list': { input: SessionsHistoryListInput; output: HistoryPageView };
  /** Search this session's normalized records for literal text, case insensitively, paging the same way `sessions.history.list` does. */
  'sessions.history.search': { input: SessionsHistorySearchInput; output: HistoryPageView };
  /** Whether the embedded editor (code-server) is enabled and runnable on this host, so a client can decide whether to offer it. */
  'sessions.ide_info': { input: SessionsIdeInfoInput; output: SessionIdeInfoView };
  /** Interrupt a session's active turn. */
  'sessions.interrupt': { input: SessionsInterruptInput; output: SessionInterruptResult };
  /** Launch a child session from a task or claimed work item. */
  'sessions.launch': { input: SessionsLaunchInput; output: SessionView };
  /** Resolve a launch selection to its exact non-secret template snapshot — agent, model, effort, protocol, mode, capacity, and provenance — without launching a session. `loom sessions launch` runs this as a canonical preflight; not exposed as its own CLI verb since callers reach it through that preview instead. */
  'sessions.launches.resolve': { input: SessionsLaunchesResolveInput; output: ResolvedLaunchView };
  /** List and search visible sessions. */
  'sessions.list': { input: SessionsListInput; output: SessionView[] };
  /** Change an ACP session's permission mode (`session/set_mode`), journaling a `mode_change` block. */
  'sessions.mode': { input: SessionsModeInput; output: SessionModeResult };
  /** Answer a pending in-flight ACP permission prompt by its chosen option: 404 for an unknown request id, 409 when it was already resolved. */
  'sessions.permissions.answer': { input: SessionsPermissionsAnswerInput; output: AnswerPermissionResult };
  /** Read a bounded terminal preview. */
  'sessions.preview': { input: SessionsPreviewInput; output: SessionPreviewResult };
  /** Send a user message to an ACP session. Dispatched immediately when idle, or appended to the durable queue while a turn is live; `send_now` instead cancels any live turn and starts the message as a normal prompt. Every send records a `nudge` event on the branch (the audit rule). */
  'sessions.prompt.create': { input: SessionsPromptCreateInput; output: PromptResult };
  /** Pull unseen next-turn feedback back out of the durable queue for editing. The ACP task owns the consume so this action is serialized with automatic dispatch at a turn boundary. */
  'sessions.prompt.retract': { input: SessionsPromptRetractInput; output: RetractResult };
  /** Raw bytes of a worktree file, with a guessed content type — for inline image previews and downloads. Always reads the working tree, never a git ref. */
  'sessions.raw': { input: SessionsRawInput; output: null };
  /** Recover an archived session: rebuild its worktree from the kept branch, then resume the agent. For a live (non-archived) session, restart its ACP runtime instead. The inverse of `archive`. */
  'sessions.recover': { input: SessionsRecoverInput; output: SessionView };
  /** Generate the session's resumption cue if it is missing or stale. `force` regenerates it unconditionally; otherwise the configured inactivity threshold applies, as on the on-return path. */
  'sessions.resumption_cue.ensure': { input: SessionsResumptionCueEnsureInput; output: ResumptionCueView };
  /** The session's current resumption cue, if one has been generated. */
  'sessions.resumption_cue.get': { input: SessionsResumptionCueGetInput; output: ResumptionCueView };
  /** Delete one Scratch file. */
  'sessions.scratch.delete': { input: SessionsScratchDeleteInput; output: ScratchDeleteResult };
  /** Shared upload limits for launch-time and live-session Scratch attachments. */
  'sessions.scratch.limits': { input: SessionsScratchLimitsInput; output: ScratchLimitsView };
  /** List a session's Scratch files. */
  'sessions.scratch.list': { input: SessionsScratchListInput; output: ScratchFileView[] };
  /** Write one Scratch file from a raw request body. */
  'sessions.scratch.write': { input: SessionsScratchWriteInput; output: ScratchWriteResult };
  /** Deliver a new prompt to a session. */
  'sessions.send': { input: SessionsSendInput; output: SessionSendResult };
  /** Close one of a session's worktree debug shells, killing its supervisor. */
  'sessions.shells.delete': { input: SessionsShellsDeleteInput; output: number[] };
  /** The live worktree debug-shell indices for a session, so a client re-opens the shell tabs after a reload. Never spawns. */
  'sessions.shells.list': { input: SessionsShellsListInput; output: number[] };
  /** Attach to one of a session's worktree debug shells over a websocket. */
  'sessions.shells.terminal': { input: SessionsShellsTerminalInput; output: null };
  /** Read the session's durable attention level and status message. */
  'sessions.status.get': { input: SessionsStatusGetInput; output: BranchView };
  /** Update the durable attention level and status message. */
  'sessions.status.set': { input: SessionsStatusSetInput; output: BranchView };
  /** Return the current goal, status, inbox, artifacts, issues, and next actions. */
  'sessions.summary.get': { input: SessionsSummaryGetInput; output: SessionCatchupView };
  /** The fleet index: one compact row per visible session. */
  'sessions.summary.list': { input: SessionsSummaryListInput; output: SessionSummaryView[] };
  /** Remove one free-form session tag. */
  'sessions.tags.delete': { input: SessionsTagsDeleteInput; output: BranchView };
  /** List free-form tags on a session. */
  'sessions.tags.list': { input: SessionsTagsListInput; output: BranchView };
  /** Atomically replace one author's complete tag set on a session. */
  'sessions.tags.replace': { input: SessionsTagsReplaceInput; output: SessionView };
  /** Set one free-form session tag. */
  'sessions.tags.set': { input: SessionsTagsSetInput; output: BranchView };
  /** Attach to a session's agent terminal over a websocket. */
  'sessions.terminal': { input: SessionsTerminalInput; output: null };
  /** Toggle whether Loom generates this session's title automatically. */
  'sessions.title.generation.set': { input: SessionsTitleGenerationSetInput; output: SessionView };
  /** Regenerate a session's title immediately, bypassing the confidence guard that normally throttles automatic generation. */
  'sessions.title.regenerate': { input: SessionsTitleRegenerateInput; output: SessionView };
  /** Update a session's branch-level fields (title, goal, description) and its durable status. Attention level is managed via tags operations (`sessions.tags.set`/`sessions.tags.delete`). */
  'sessions.update': { input: SessionsUpdateInput; output: SessionView };
  /** The externally-visible dashboard URL for a session. The agent inside a session only knows its loopback API address, so only the server can resolve this — from the configured `auth.base_url`, or the address it is bound to. */
  'sessions.url': { input: SessionsUrlInput; output: SessionUrlView };
  /** Remove one variable from the default profile's environment. A missing name is not an error — the desired end state already holds. */
  'settings.env.delete': { input: SettingsEnvDeleteInput; output: AgentEnvVarView[] };
  /** List every variable in the default profile's environment. Unlike a named profile's environment metadata, values are returned in full. */
  'settings.env.list': { input: SettingsEnvListInput; output: AgentEnvVarView[] };
  /** Upsert one variable in the default profile's environment. The value is free-form; the name is validated as a shell identifier so it cannot corrupt the launch script that exports it. */
  'settings.env.set': { input: SettingsEnvSetInput; output: AgentEnvVarView[] };
  /** Every registered runtime setting and its effective value. */
  'settings.get': { input: SettingsGetInput; output: SettingsEnvelope };
  /** Apply setting changes. A `null` value clears a key back to its default. */
  'settings.patch': { input: SettingsPatchInput; output: SettingsEnvelope };
  /** Restart the standalone operator shell, discarding its process state. */
  'shell.restart': { input: ShellRestartInput; output: ShellRestartResult };
  /** Attach to the standalone operator shell over a websocket. */
  'shell.terminal': { input: ShellTerminalInput; output: null };
  /** Slack integration status: which credentials are set, whether `auth.test` resolves a live bot identity, the configured access boundary, and the Socket Mode supervisor's live health. */
  'slack.connection_status': { input: SlackConnectionStatusInput; output: SlackConnectionStatusOutput };
  /** List recent detached background tasks — currently the GitHub `@loom` trigger launches, which run off the webhook request so a slow clone can't blow GitHub's delivery timeout — newest first. */
  'tasks.list': { input: TasksListInput; output: TaskView[] };
  /** Register a watch. */
  'watches.create': { input: WatchesCreateInput; output: WatchView };
  /** Remove a watch. */
  'watches.delete': { input: WatchesDeleteInput; output: WatchDeleteResult };
  /** Inspect one watch by id or name. */
  'watches.get': { input: WatchesGetInput; output: WatchView };
  /** List every registered watch: name, enabled, trigger, program, last outcome. */
  'watches.list': { input: WatchesListInput; output: WatchView[] };
  /** List the builtin watch programs that ship with loom. */
  'watches.programs': { input: WatchesProgramsInput; output: ProgramView[] };
  /** Fire a watch round now, in the daemon, and report its outcome. `dry_run` stubs every mutating action — the iteration primitive, safe to repeat. */
  'watches.run': { input: WatchesRunInput; output: WatchRunResult };
  /** Show a watch's round history: time, trigger reason, outcome, summary, and the captured stdout/stderr/exit status of each round. */
  'watches.runs': { input: WatchesRunsInput; output: WatchRunView[] };
  /** Update a watch's settings, optionally arm or disarm it via the `enabled` field. */
  'watches.update': { input: WatchesUpdateInput; output: WatchView };
}

export type OperationId = keyof Operations;
export type OperationInput<K extends OperationId> = Operations[K]['input'];
export type OperationOutput<K extends OperationId> = Operations[K]['output'];

/** Each operation's canonical route, derived in Rust from its identity by
 * `OperationSpec::path`. The frontend reads it rather than deriving it a
 * second time. */
export const OPERATION_ROUTES = {
  'agents.custom.create': { method: 'POST', path: '/api/agents/custom/create' },
  'agents.custom.delete': { method: 'POST', path: '/api/agents/custom/delete' },
  'agents.custom.update': { method: 'POST', path: '/api/agents/custom/update' },
  'agents.list': { method: 'POST', path: '/api/agents/list' },
  'agents.oneshot': { method: 'POST', path: '/api/agents/oneshot' },
  'artifacts.delete': { method: 'POST', path: '/api/artifacts/delete' },
  'artifacts.get': { method: 'POST', path: '/api/artifacts/get' },
  'artifacts.history': { method: 'POST', path: '/api/artifacts/history' },
  'artifacts.list': { method: 'POST', path: '/api/artifacts/list' },
  'artifacts.raw': { method: 'GET', path: '/api/artifacts/raw' },
  'artifacts.threads.comment': { method: 'POST', path: '/api/artifacts/threads/comment' },
  'artifacts.threads.list': { method: 'POST', path: '/api/artifacts/threads/list' },
  'artifacts.threads.resolve': { method: 'POST', path: '/api/artifacts/threads/resolve' },
  'artifacts.url': { method: 'POST', path: '/api/artifacts/url' },
  'artifacts.write': { method: 'POST', path: '/api/artifacts/write' },
  'auth.automation_token': { method: 'POST', path: '/api/auth/automation_token' },
  'auth.federate': { method: 'POST', path: '/api/auth/federate' },
  'auth.federations.create': { method: 'POST', path: '/api/auth/federations/create' },
  'auth.federations.list': { method: 'POST', path: '/api/auth/federations/list' },
  'auth.federations.remove': { method: 'POST', path: '/api/auth/federations/remove' },
  'auth.github_config.get': { method: 'POST', path: '/api/auth/github_config/get' },
  'auth.github_config.set': { method: 'POST', path: '/api/auth/github_config/set' },
  'auth.github_token.get': { method: 'POST', path: '/api/auth/github_token/get' },
  'auth.github_token.remove': { method: 'POST', path: '/api/auth/github_token/remove' },
  'auth.github_token.set': { method: 'POST', path: '/api/auth/github_token/set' },
  'auth.login': { method: 'POST', path: '/api/auth/login' },
  'auth.logout': { method: 'POST', path: '/api/auth/logout' },
  'auth.me': { method: 'POST', path: '/api/auth/me' },
  'auth.set_password': { method: 'POST', path: '/api/auth/set_password' },
  'auth.tokens.create': { method: 'POST', path: '/api/auth/tokens/create' },
  'auth.tokens.list': { method: 'POST', path: '/api/auth/tokens/list' },
  'auth.tokens.revoke': { method: 'POST', path: '/api/auth/tokens/revoke' },
  'auth.users.create': { method: 'POST', path: '/api/auth/users/create' },
  'auth.users.list': { method: 'POST', path: '/api/auth/users/list' },
  'auth.users.remove': { method: 'POST', path: '/api/auth/users/remove' },
  'auth.users.set_role': { method: 'POST', path: '/api/auth/users/set_role' },
  'branches.events.create': { method: 'POST', path: '/api/branches/events/create' },
  'branches.events.list': { method: 'POST', path: '/api/branches/events/list' },
  'branches.get': { method: 'POST', path: '/api/branches/get' },
  'branches.issues.list': { method: 'POST', path: '/api/branches/issues/list' },
  'branches.list': { method: 'POST', path: '/api/branches/list' },
  'branches.slack.reply': { method: 'POST', path: '/api/branches/slack/reply' },
  'branches.status.set': { method: 'POST', path: '/api/branches/status/set' },
  'branches.tags.delete': { method: 'POST', path: '/api/branches/tags/delete' },
  'branches.tags.set': { method: 'POST', path: '/api/branches/tags/set' },
  'branches.update': { method: 'POST', path: '/api/branches/update' },
  'channels.archive': { method: 'POST', path: '/api/channels/archive' },
  'channels.bindings.list': { method: 'POST', path: '/api/channels/bindings/list' },
  'channels.create': { method: 'POST', path: '/api/channels/create' },
  'channels.get': { method: 'POST', path: '/api/channels/get' },
  'channels.list': { method: 'POST', path: '/api/channels/list' },
  'channels.messages.create': { method: 'POST', path: '/api/channels/messages/create' },
  'channels.messages.list': { method: 'POST', path: '/api/channels/messages/list' },
  'channels.read_marker.set': { method: 'POST', path: '/api/channels/read_marker/set' },
  'channels.subscription.set': { method: 'POST', path: '/api/channels/subscription/set' },
  'channels.wait': { method: 'POST', path: '/api/channels/wait' },
  'deployment.reconcile': { method: 'POST', path: '/api/deployment/reconcile' },
  'diagnostics.get': { method: 'POST', path: '/api/diagnostics/get' },
  'diagnostics.status': { method: 'POST', path: '/api/diagnostics/status' },
  'events.stream': { method: 'GET', path: '/api/events/stream' },
  'issues.actions': { method: 'POST', path: '/api/issues/actions' },
  'issues.backlog.create': { method: 'POST', path: '/api/issues/backlog/create' },
  'issues.board': { method: 'POST', path: '/api/issues/board' },
  'issues.close': { method: 'POST', path: '/api/issues/close' },
  'issues.create': { method: 'POST', path: '/api/issues/create' },
  'issues.delete': { method: 'POST', path: '/api/issues/delete' },
  'issues.get': { method: 'POST', path: '/api/issues/get' },
  'issues.list': { method: 'POST', path: '/api/issues/list' },
  'issues.reopen': { method: 'POST', path: '/api/issues/reopen' },
  'issues.tags.delete': { method: 'POST', path: '/api/issues/tags/delete' },
  'issues.tags.set': { method: 'POST', path: '/api/issues/tags/set' },
  'issues.update': { method: 'POST', path: '/api/issues/update' },
  'logs.list': { method: 'POST', path: '/api/logs/list' },
  'logs.stream': { method: 'GET', path: '/api/logs/stream' },
  'mcps.custom.create': { method: 'POST', path: '/api/mcps/custom/create' },
  'mcps.custom.delete': { method: 'POST', path: '/api/mcps/custom/delete' },
  'mcps.custom.get': { method: 'POST', path: '/api/mcps/custom/get' },
  'mcps.custom.list': { method: 'POST', path: '/api/mcps/custom/list' },
  'mcps.custom.update': { method: 'POST', path: '/api/mcps/custom/update' },
  'mcps.get': { method: 'POST', path: '/api/mcps/get' },
  'permissions.effective.get': { method: 'POST', path: '/api/permissions/effective/get' },
  'permissions.explain': { method: 'POST', path: '/api/permissions/explain' },
  'permissions.github.grant': { method: 'POST', path: '/api/permissions/github/grant' },
  'permissions.github.restricted.invoke': { method: 'POST', path: '/api/permissions/github/restricted/invoke' },
  'permissions.github.revoke': { method: 'POST', path: '/api/permissions/github/revoke' },
  'permissions.github.token': { method: 'POST', path: '/api/permissions/github/token' },
  'permissions.requests.approve': { method: 'POST', path: '/api/permissions/requests/approve' },
  'permissions.requests.create': { method: 'POST', path: '/api/permissions/requests/create' },
  'permissions.requests.deny': { method: 'POST', path: '/api/permissions/requests/deny' },
  'permissions.requests.list': { method: 'POST', path: '/api/permissions/requests/list' },
  'preferences.get': { method: 'POST', path: '/api/preferences/get' },
  'preferences.patch': { method: 'POST', path: '/api/preferences/patch' },
  'profiles.clone': { method: 'POST', path: '/api/profiles/clone' },
  'profiles.create': { method: 'POST', path: '/api/profiles/create' },
  'profiles.delete': { method: 'POST', path: '/api/profiles/delete' },
  'profiles.effective': { method: 'POST', path: '/api/profiles/effective' },
  'profiles.env.delete': { method: 'POST', path: '/api/profiles/env/delete' },
  'profiles.env.set': { method: 'POST', path: '/api/profiles/env/set' },
  'profiles.get': { method: 'POST', path: '/api/profiles/get' },
  'profiles.list': { method: 'POST', path: '/api/profiles/list' },
  'profiles.update': { method: 'POST', path: '/api/profiles/update' },
  'repos.branches': { method: 'POST', path: '/api/repos/branches' },
  'repos.env.delete': { method: 'POST', path: '/api/repos/env/delete' },
  'repos.env.get': { method: 'POST', path: '/api/repos/env/get' },
  'repos.env.set': { method: 'POST', path: '/api/repos/env/set' },
  'repos.list': { method: 'POST', path: '/api/repos/list' },
  'repos.recent': { method: 'POST', path: '/api/repos/recent' },
  'repos.register': { method: 'POST', path: '/api/repos/register' },
  'repos.revisions.validate': { method: 'POST', path: '/api/repos/revisions/validate' },
  'reviews.comments.create': { method: 'POST', path: '/api/reviews/comments/create' },
  'reviews.comments.delete': { method: 'POST', path: '/api/reviews/comments/delete' },
  'reviews.comments.resolve': { method: 'POST', path: '/api/reviews/comments/resolve' },
  'reviews.comments.update': { method: 'POST', path: '/api/reviews/comments/update' },
  'reviews.create': { method: 'POST', path: '/api/reviews/create' },
  'reviews.discard': { method: 'POST', path: '/api/reviews/discard' },
  'reviews.get': { method: 'POST', path: '/api/reviews/get' },
  'reviews.list': { method: 'POST', path: '/api/reviews/list' },
  'reviews.retarget': { method: 'POST', path: '/api/reviews/retarget' },
  'reviews.retry_delivery': { method: 'POST', path: '/api/reviews/retry_delivery' },
  'reviews.submit': { method: 'POST', path: '/api/reviews/submit' },
  'reviews.update': { method: 'POST', path: '/api/reviews/update' },
  'runs.create': { method: 'POST', path: '/api/runs/create' },
  'runs.get': { method: 'POST', path: '/api/runs/get' },
  'runs.list': { method: 'POST', path: '/api/runs/list' },
  'session_layout.defaults.delete': { method: 'POST', path: '/api/session_layout/defaults/delete' },
  'session_layout.defaults.set': { method: 'POST', path: '/api/session_layout/defaults/set' },
  'session_layout.events': { method: 'GET', path: '/api/session_layout/events' },
  'session_layout.get': { method: 'POST', path: '/api/session_layout/get' },
  'session_layout.groups.create': { method: 'POST', path: '/api/session_layout/groups/create' },
  'session_layout.groups.delete': { method: 'POST', path: '/api/session_layout/groups/delete' },
  'session_layout.groups.preference.set': { method: 'POST', path: '/api/session_layout/groups/preference/set' },
  'session_layout.groups.update': { method: 'POST', path: '/api/session_layout/groups/update' },
  'session_layout.move': { method: 'POST', path: '/api/session_layout/move' },
  'session_layout.reorder': { method: 'POST', path: '/api/session_layout/reorder' },
  'session_layout.restore': { method: 'POST', path: '/api/session_layout/restore' },
  'session_layout.spaces.create': { method: 'POST', path: '/api/session_layout/spaces/create' },
  'session_layout.spaces.delete': { method: 'POST', path: '/api/session_layout/spaces/delete' },
  'session_layout.spaces.update': { method: 'POST', path: '/api/session_layout/spaces/update' },
  'sessions.adopt': { method: 'POST', path: '/api/sessions/adopt' },
  'sessions.archive': { method: 'POST', path: '/api/sessions/archive' },
  'sessions.changes': { method: 'POST', path: '/api/sessions/changes' },
  'sessions.chat': { method: 'POST', path: '/api/sessions/chat' },
  'sessions.chat.stream': { method: 'GET', path: '/api/sessions/chat/stream' },
  'sessions.config.set': { method: 'POST', path: '/api/sessions/config/set' },
  'sessions.context': { method: 'POST', path: '/api/sessions/context' },
  'sessions.conversation': { method: 'POST', path: '/api/sessions/conversation' },
  'sessions.conversation.block': { method: 'POST', path: '/api/sessions/conversation/block' },
  'sessions.delete': { method: 'POST', path: '/api/sessions/delete' },
  'sessions.events.create': { method: 'POST', path: '/api/sessions/events/create' },
  'sessions.events.list': { method: 'POST', path: '/api/sessions/events/list' },
  'sessions.events.stream': { method: 'GET', path: '/api/sessions/events/stream' },
  'sessions.files': { method: 'POST', path: '/api/sessions/files' },
  'sessions.get': { method: 'POST', path: '/api/sessions/get' },
  'sessions.github.access.list': { method: 'POST', path: '/api/sessions/github/access/list' },
  'sessions.github.clear': { method: 'POST', path: '/api/sessions/github/clear' },
  'sessions.github.labels.add': { method: 'POST', path: '/api/sessions/github/labels/add' },
  'sessions.github.refresh': { method: 'POST', path: '/api/sessions/github/refresh' },
  'sessions.github.set': { method: 'POST', path: '/api/sessions/github/set' },
  'sessions.handoff': { method: 'POST', path: '/api/sessions/handoff' },
  'sessions.handoff.resolve': { method: 'POST', path: '/api/sessions/handoff/resolve' },
  'sessions.history.list': { method: 'POST', path: '/api/sessions/history/list' },
  'sessions.history.search': { method: 'POST', path: '/api/sessions/history/search' },
  'sessions.ide_info': { method: 'POST', path: '/api/sessions/ide_info' },
  'sessions.interrupt': { method: 'POST', path: '/api/sessions/interrupt' },
  'sessions.launch': { method: 'POST', path: '/api/sessions/launch' },
  'sessions.launches.resolve': { method: 'POST', path: '/api/sessions/launches/resolve' },
  'sessions.list': { method: 'POST', path: '/api/sessions/list' },
  'sessions.mode': { method: 'POST', path: '/api/sessions/mode' },
  'sessions.permissions.answer': { method: 'POST', path: '/api/sessions/permissions/answer' },
  'sessions.preview': { method: 'POST', path: '/api/sessions/preview' },
  'sessions.prompt.create': { method: 'POST', path: '/api/sessions/prompt/create' },
  'sessions.prompt.retract': { method: 'POST', path: '/api/sessions/prompt/retract' },
  'sessions.raw': { method: 'GET', path: '/api/sessions/raw' },
  'sessions.recover': { method: 'POST', path: '/api/sessions/recover' },
  'sessions.resumption_cue.ensure': { method: 'POST', path: '/api/sessions/resumption_cue/ensure' },
  'sessions.resumption_cue.get': { method: 'POST', path: '/api/sessions/resumption_cue/get' },
  'sessions.scratch.delete': { method: 'POST', path: '/api/sessions/scratch/delete' },
  'sessions.scratch.limits': { method: 'POST', path: '/api/sessions/scratch/limits' },
  'sessions.scratch.list': { method: 'POST', path: '/api/sessions/scratch/list' },
  'sessions.scratch.write': { method: 'POST', path: '/api/sessions/scratch/write' },
  'sessions.send': { method: 'POST', path: '/api/sessions/send' },
  'sessions.shells.delete': { method: 'POST', path: '/api/sessions/shells/delete' },
  'sessions.shells.list': { method: 'POST', path: '/api/sessions/shells/list' },
  'sessions.shells.terminal': { method: 'GET', path: '/api/sessions/shells/terminal' },
  'sessions.status.get': { method: 'POST', path: '/api/sessions/status/get' },
  'sessions.status.set': { method: 'POST', path: '/api/sessions/status/set' },
  'sessions.summary.get': { method: 'POST', path: '/api/sessions/summary/get' },
  'sessions.summary.list': { method: 'POST', path: '/api/sessions/summary/list' },
  'sessions.tags.delete': { method: 'POST', path: '/api/sessions/tags/delete' },
  'sessions.tags.list': { method: 'POST', path: '/api/sessions/tags/list' },
  'sessions.tags.replace': { method: 'POST', path: '/api/sessions/tags/replace' },
  'sessions.tags.set': { method: 'POST', path: '/api/sessions/tags/set' },
  'sessions.terminal': { method: 'GET', path: '/api/sessions/terminal' },
  'sessions.title.generation.set': { method: 'POST', path: '/api/sessions/title/generation/set' },
  'sessions.title.regenerate': { method: 'POST', path: '/api/sessions/title/regenerate' },
  'sessions.update': { method: 'POST', path: '/api/sessions/update' },
  'sessions.url': { method: 'POST', path: '/api/sessions/url' },
  'settings.env.delete': { method: 'POST', path: '/api/settings/env/delete' },
  'settings.env.list': { method: 'POST', path: '/api/settings/env/list' },
  'settings.env.set': { method: 'POST', path: '/api/settings/env/set' },
  'settings.get': { method: 'POST', path: '/api/settings/get' },
  'settings.patch': { method: 'POST', path: '/api/settings/patch' },
  'shell.restart': { method: 'POST', path: '/api/shell/restart' },
  'shell.terminal': { method: 'GET', path: '/api/shell/terminal' },
  'slack.connection_status': { method: 'POST', path: '/api/slack/connection_status' },
  'tasks.list': { method: 'POST', path: '/api/tasks/list' },
  'watches.create': { method: 'POST', path: '/api/watches/create' },
  'watches.delete': { method: 'POST', path: '/api/watches/delete' },
  'watches.get': { method: 'POST', path: '/api/watches/get' },
  'watches.list': { method: 'POST', path: '/api/watches/list' },
  'watches.programs': { method: 'POST', path: '/api/watches/programs' },
  'watches.run': { method: 'POST', path: '/api/watches/run' },
  'watches.runs': { method: 'POST', path: '/api/watches/runs' },
  'watches.update': { method: 'POST', path: '/api/watches/update' },
} as const satisfies Record<OperationId, { method: string; path: string }>;
