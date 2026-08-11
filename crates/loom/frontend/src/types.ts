/** One (key, value) annotation on a branch. Loudness lives in the VALUE: a tag
 *  whose value is on the `attention | blocked` ladder is *loud* (raises a badge)
 *  regardless of key — the agent's own `attention` self-report and a watch's
 *  typed marks (`review`, `stuck`, …) alike. The key is the type (the chip
 *  label); every other value is a quiet, free-form pill. Absence is the calm
 *  state — there is no stored `ok`. Mirrors weaver-api's `TagView`. */
export interface Tag {
  key: string;
  value: string;
  /** One-line reason accompanying the tag. */
  note: string;
  /** Who set it — `agent`, a watch/watch name, or `manual`. */
  set_by: string;
  /** When it was last set (ISO). The dashboard fades an outside mark stale once
   *  the session's activity advances past this. */
  set_at: string;
}

export interface ChannelDelivery {
  target_session_id: string;
  state: 'queued' | 'delivered' | 'failed';
  attempts: number;
  last_error: string | null;
  updated_at: string;
}

export interface ChannelMessage {
  id: string;
  channel_id: string;
  seq: number;
  kind: 'goal' | 'message' | 'status' | 'result' | 'system';
  urgency: 'normal' | 'attention' | 'blocked';
  author_kind: string;
  author_id: string;
  body: string;
  payload: unknown;
  reply_to: string | null;
  created_at: string;
  deliveries: ChannelDelivery[];
}

export interface Channel {
  id: string;
  kind: 'session' | 'custom';
  repo_root: string;
  branch_id: string | null;
  session_id: string | null;
  name: string;
  topic: string;
  state: 'open' | 'archived';
  created_by_kind: string;
  created_by: string;
  created_at: string;
  archived_at: string | null;
  unread_count: number;
  unread_urgent_count: number;
  last_message: ChannelMessage | null;
}

export interface ChannelSubscription {
  channel_id: string;
  subject_kind: string;
  subject_id: string;
  mode: 'observe' | 'deliver';
  read_seq: number;
  created_at: string;
  updated_at: string;
}

/** A branch is the engine's view of "what the agent is working on": one
 *  `(repo_root, branch)` pair with a goal, a title, and a free-form
 *  description. Branches are owned by `weaver-core` and exist whether or not
 *  loom is running. */
export interface Branch {
  id: string;
  /** Short label: the branch name with the optional `weaver/` prefix stripped. */
  name: string;
  title: string;
  title_provenance: 'derived' | 'generated' | 'user' | 'issue';
  goal: string;
  /** The agent's current-state message, set together with the `attention` tag
   *  via `weaver status` (e.g. "Wired up routes; tests pass"). Shown even
   *  when the branch is calm. */
  description: string;
  /** Every tag on the branch: the agent's own loud `attention`, a watch's typed
   *  marks, and any free-form quiet key. Empty when the branch is calm and
   *  unmarked — absence is the default state, there is no `ok` tag. */
  tags: Tag[];
  repo_root: string;
  branch: string;
  base_branch: string;
  created_at: string;
  updated_at: string;
  open_issue_count: number;
  /** Latest GitHub pull-request snapshot for the branch, or null when GitHub
   *  polling is off, there's no PR, or `gh` is unavailable. Maintained
   *  server-side by loom's poll loop. */
  github: GithubStatus | null;
  /** Explicit PR override. null means automatic current-open-PR discovery. */
  github_pr: number | null;
}

/** A point-in-time GitHub snapshot of a branch's pull request: its link plus
 *  the review and check rollups loom read via the `gh` CLI. */
export interface GithubStatus {
  pr_number: number;
  pr_url: string;
  /** 'OPEN' | 'CLOSED' | 'MERGED'. */
  pr_state: string;
  pr_title: string;
  is_draft: boolean;
  /** 'APPROVED' | 'CHANGES_REQUESTED' | 'REVIEW_REQUIRED' | null. */
  review_decision: string | null;
  /** Rolled-up checks: 'passing' | 'failing' | 'pending' | null (no checks). */
  checks: string | null;
  /** 'MERGEABLE' | 'CONFLICTING' | 'UNKNOWN' | null. */
  mergeable: string | null;
  merged_at: string | null;
  /** Current GitHub PR head and the update time associated with that head. The
   * time stays fixed across metadata-only PR updates. */
  head_sha: string | null;
  head_updated_at: string | null;
  fetched_at: string;
}

/** A session is loom's view: one terminal + one running agent attached to a
 *  branch. Branch-level fields live under `branch`. */
export interface Session {
  id: string;
  status: string;
  transition: SessionTransition | null;
  work_dir: string;
  term_session: string;
  agent_kind: string;
  /** Model selector interpreted by the selected agent protocol. */
  model: string;
  /** Reasoning effort interpreted by the selected agent protocol. */
  effort: string;
  github_repo: string | null;
  /** GitHub issue linked through this session's explicit work item. */
  github_issue: { repo: string; number: number } | null;
  last_activity_at: string;
  created_at: string;
  updated_at: string;
  title_generation: {
    enabled: boolean;
    status:
      | 'idle'
      | 'running'
      | 'generated'
      | 'protected'
      | 'disabled'
      | 'unavailable'
      | 'stale'
      | 'failed';
  };
  /** Branch id of the session that launched this one (its parent in the session
   *  tree), or null for a top-level session. The dashboard groups the list into
   *  threads by it; a child whose parent is absent (archived, or never tracked)
   *  renders at the top level. Stamped on the session row at launch. */
  parent_id: string | null;
  /** Exact session id of the launcher. Prefer this for navigation; parent_id is
   *  only a legacy branch-ancestry fallback for older rows. */
  parent_session_id: string | null;
  /** The principal (username) that launched this session — attribution for the
   *  shared team board. null for engine-created warm watch sessions and rows
   *  predating the column. A tracking/UX field, not
   *  a security boundary: the fleet stays co-owned by everyone authenticated. */
  created_by: string | null;
  /** Explicit claimed/imported compatibility work item. Ordinary sessions use
   *  their same-id channel and leave this null. */
  tracking_issue: number | null;
  /** Who/what launched this session: `user` (the New Session drawer or the CLI)
   *  or an automation surface — `agent`, `github`, `slack`, `watch`, `actions`,
   *  `ops`. Drives the origin pill on an automation-class row. */
  origin: string;
  /** `interactive` (a person's own session) or `automation` (agent/system
   *  launched). Both are normal workbench sessions; class remains a machine
   *  fact used by recursion/issue policies. */
  class: string;
  /** Total agent turns run so far. */
  turn_count: number;
  /** Legacy compatibility field. New clients use durable group placement;
   *  explicit parked rows are migrated to a Later group. */
  park: 'parked' | 'active' | null;
  /** Legacy compatibility field. New clients use placement rank. */
  sort_order: number | null;
  /** Execution backend: `'terminal'` (a PTY + interactive TUI) or `'acp'` (a
   *  headless adapter driven over the Agent Client Protocol). Older/terminal rows
   *  read as `'terminal'`. The Conversation surface renders from the chat journal
   *  when this is `'acp'`, and from the iris scrape otherwise. */
  protocol: 'terminal' | 'acp';
  /** The agent's own on-disk ACP session id for an `acp` session, or null. */
  acp_session_id: string | null;
  /** The current ACP mode id (gating posture: `bypassPermissions`, `auto`,
   *  `acceptEdits`, `default`, `plan`), or null for a terminal session / before
   *  one is set. */
  current_mode: string | null;
  /** The latest context-window usage the current ACP provider reported, or null. */
  usage: AcpUsage | null;
  profile: string;
  profile_revision: number;
  profile_lifetime: number;
  policy_strict: boolean;
  mutation_revision: number;
  launch_mode: string;
  /** Immutable, source-redacted launch resolution stamped by the server. */
  resolved_launch: ResolvedLaunch | null;
  /** Exact capability snapshot stamped at launch. Custom source is redacted. */
  mcp_policy: SessionMcpPolicy;
  /** One canonical, shared workbench placement. */
  placement: SessionPlacement | null;
  branch: Branch;
}

/** Compact branch projection used by the fleet polling/search endpoint. Large
 * goal text and detail-only metadata remain on `Branch`. */
export interface BranchSummary {
  id: string;
  name: string;
  title: string;
  description: string;
  tags: Tag[];
  repo_root: string;
  branch: string;
  github: GithubStatus | null;
  github_pr: number | null;
}

/** Compact session projection returned by `GET /sessions/summary`. Opening a
 * row or session follows with the full `Session` resource on demand. */
export interface SessionSummary {
  id: string;
  status: string;
  transition: SessionTransition | null;
  github_repo: string | null;
  github_issue: { repo: string; number: number } | null;
  last_activity_at: string;
  created_at: string;
  parent_id: string | null;
  parent_session_id: string | null;
  created_by: string | null;
  origin: string;
  class: string;
  tracking_issue: number | null;
  profile: string;
  usage: AcpUsage | null;
  placement: SessionPlacement | null;
  branch: BranchSummary;
}

export interface SessionTransition {
  kind: 'archiving' | 'adopting';
  step: string;
  started_at: string;
}

export interface ResumptionEvidence {
  kind: 'conversation' | 'artifact';
  label: string;
  href: string;
  cursor: string;
}

export interface ResumptionCue {
  status: 'generated' | 'generating' | 'due' | 'not_due' | 'disabled' | 'unavailable';
  source_cursor: string | null;
  text: string | null;
  generated_at: string | null;
  evidence: ResumptionEvidence[];
}

export interface SessionPlacement {
  session_id: string;
  group_id: string;
  group_name: string;
  group_system_key: string | null;
  space_id: string;
  space_name: string;
  rank: number;
}

export interface SessionGroup {
  id: string;
  space_id: string;
  name: string;
  rank: number;
  system_key: string | null;
  collapsed: boolean;
  /** Includes archived session ids; each view projects its own live/history scope. */
  session_ids: string[];
}

export interface SessionSpace {
  id: string;
  name: string;
  rank: number;
  system_key: string | null;
  groups: SessionGroup[];
}

export type SessionPlacementSelectorKind = 'origin' | 'profile' | 'watch';
export type SessionLayoutItemKind = 'space' | 'group';

export interface SessionPlacementDefault {
  selector_kind: SessionPlacementSelectorKind;
  selector_value: string;
  group_id: string;
}

export interface SessionLayout {
  revision: number;
  spaces: SessionSpace[];
  defaults: SessionPlacementDefault[];
}

export interface SessionGroupOrder {
  group_id: string;
  session_ids: string[];
}

export type SessionSearchStatus =
  'created' | 'running' | 'orphaned' | 'done' | 'error' | 'archived';
export type SessionSearchAttention = 'needs' | 'ok' | 'attention' | 'blocked';
export type SessionCreatorFilter = 'mine' | 'ops' | 'mine-and-ops' | 'other-users';
export interface SessionSearchOptions {
  history?: boolean;
  archivedOnly?: boolean;
  status?: SessionSearchStatus;
  attention?: SessionSearchAttention;
  creator?: SessionCreatorFilter;
}

export type AutomationRunStatus =
  'creating' | 'waiting' | 'delivering' | 'running' | 'failed' | 'cancelled' | 'completed';

/** Durable automation launch reservation (`GET /api/runs`). A run normally
 *  points at an automation-class Session, but a launch can fail before that
 *  session becomes usable; unmatched failures become typed interventions. */
export interface AutomationRun {
  id: string;
  actor_subject: string;
  source: string;
  watch_id: string | null;
  service_tag: string;
  profile: string;
  idempotency_key: string;
  channel: string | null;
  session_id: string;
  status: AutomationRunStatus;
  outcome: string | null;
  summary: string;
  created_at: string;
  updated_at: string;
}

// ── ACP conversation surface ────────────────────────────────────────────────
// The chat journal + live SSE tail an `acp` session's Conversation renders from.
// These hand-mirror loom's `chat.rs` / `acp/mod.rs` serde shapes: the block
// contract (`GET /sessions/{id}/chat`) and the `/chat/stream` SSE events.

export interface AcpCost {
  amount: number;
  currency: string;
}

/** Context-window usage (tokens), plus optional cumulative session cost. */
export interface AcpUsage {
  used: number;
  size: number;
  cost?: AcpCost | null;
}

/** The closed set of chat-journal block kinds. */
export type ChatBlockKind =
  | 'user_message'
  | 'agent_message'
  | 'thought'
  | 'tool_call'
  | 'plan'
  | 'permission_request'
  | 'mode_change'
  | 'usage'
  | 'turn_end'
  | 'handoff';

/** One journaled block. `payload` is opaque JSON keyed by `kind` (the payload
 *  interfaces below). Addressed by `(turn, seq)`. Mirrors `chat::ChatBlockView`. */
export interface ChatBlock {
  turn: number;
  seq: number;
  kind: ChatBlockKind;
  payload: Record<string, unknown>;
  created_at: string;
}

/** `GET /sessions/{id}/chat` — the journal snapshot plus the in-flight turn (the
 *  turn number of a `session/prompt` still running, else null). */
export interface ChatSnapshot {
  blocks: ChatBlock[];
  /** Oldest loaded block, used as an exclusive cursor for an older history
   * page. Null means this snapshot reached the start of the journal. */
  older_cursor: { turn: number; seq: number } | null;
  live_turn: number | null;
  /** Permission posture captured when the live turn started. A different selected
   *  mode is queued for the next turn. */
  effective_mode: string | null;
  pending_prompt: string | null;
  metadata: AcpMetadata;
}

/** Agent-owned command/configuration metadata for a live ACP conversation. */
export interface AcpCommand {
  name: string;
  description: string;
  input?: { type?: string; hint?: string } | null;
}
export interface AcpConfigChoice {
  value: string;
  name: string;
  description?: string | null;
}
export interface AcpConfigGroup {
  group: string;
  name: string;
  options: AcpConfigChoice[];
}
export interface AcpConfigOption {
  id: string;
  name: string;
  description?: string | null;
  category?: string | null;
  type: string;
  currentValue: string | boolean;
  options?: AcpConfigChoice[] | AcpConfigGroup[];
}
export interface AcpMode {
  id: string;
  name: string;
  description?: string | null;
}
export interface AcpMetadata {
  commands: AcpCommand[];
  config_options: AcpConfigOption[];
  modes: AcpMode[];
  /** Adapter-advertised support for the private steering extension. */
  steering_supported: boolean;
}

// -- block payloads (by kind) --
export interface UserMessagePayload {
  text: string;
  by: string | null;
  steered?: boolean;
}
export interface AgentMessagePayload {
  text: string;
}
export interface ThoughtPayload {
  text: string;
  ms: number | null;
}
export interface ToolTextContent {
  type: 'text';
  text: string;
}
export interface ToolDiffContent {
  type: 'diff';
  path: string;
  old: string | null;
  new: string;
}
export interface ToolImageContent {
  type: 'image';
  data: string;
  mime_type: string;
  uri: string | null;
}
export type ToolContent = ToolTextContent | ToolDiffContent | ToolImageContent;
export interface ToolLocation {
  path: string;
  line: number | null;
}
export interface ToolCallPayload {
  tool_call_id: string;
  title: string;
  tool_kind: string;
  status: string;
  content: ToolContent[];
  locations: ToolLocation[];
}
export interface PlanEntry {
  content: string;
  status: string;
}
export interface PlanPayload {
  entries: PlanEntry[];
}
export interface PermissionOption {
  option_id: string;
  name: string;
  kind: string;
}
export interface PermissionOutcome {
  option_id?: string;
  cancelled?: boolean;
  by: string;
  at: string;
}
export interface PermissionPayload {
  request_id: string;
  tool_call_id: string | null;
  title: string;
  options: PermissionOption[];
  effective_mode?: string | null;
  outcome: PermissionOutcome | null;
}
export interface UsagePayload {
  used: number | null;
  size: number | null;
  cost?: AcpCost | null;
  reset?: boolean;
}
export interface TurnEndPayload {
  stop_reason: string;
}
export interface HandoffPayload {
  from: string;
  to: string;
  model: string;
  effort: string;
  prompt_version?: number;
  summary_status?: 'generated' | 'unavailable';
  summary_model?: string | null;
  summary?: string | null;
  through_turn?: number | null;
  through_seq?: number | null;
}

// -- `/chat/stream` SSE events --
/** `block` — a whole journaled block (upsert by `(turn, seq)`). Same shape as a
 *  snapshot block; a resolved `permission_request` re-emits its own block. */
export type SseBlock = ChatBlock;
/** `delta` — a streamed chunk of the in-flight message/thought (append to a
 *  shadow block until the whole block journals). */
export interface SseDelta {
  turn: number;
  kind: 'agent_message' | 'thought';
  text: string;
}
/** `tool` — live tool-call state, before it reaches a terminal status (then a
 *  `tool_call` block supersedes it). */
export interface SseTool {
  turn: number;
  tool_call_id: string;
  title: string;
  tool_kind: string;
  status: string;
  content: ToolContent[];
  locations: ToolLocation[];
}
/** `turn` — the turn drove live (`started`) or ended (`ended` + stop reason). */
export interface SseTurn {
  turn: number;
  state: 'started' | 'ended';
  effective_mode?: string | null;
  stop_reason?: string;
}

/** `queue` — the complete durable next-turn prompt after a queue mutation. */
export interface SseQueue {
  pending_prompt: string | null;
}

/** `POST /sessions/{id}/prompt` 202 body: whether the message steered the live
 *  turn, queued behind it, or started normally, plus the turn it belongs to. */
export interface PromptAck {
  queued: boolean;
  steered: boolean;
  turn: number | null;
}

export interface AgentChoice {
  id: string;
  label: string;
}

export interface AgentMetadata {
  kind: string;
  label: string;
  models: AgentChoice[];
  efforts: AgentChoice[];
  accepts_raw_model: boolean;
  supports_hooks: boolean;
  /** True for the builtin `claude`/`codex`; false for an operator-defined custom
   *  agent (which the UI may edit or delete). */
  builtin: boolean;
  /** Whether this runtime can replace another live ACP provider. */
  supports_acp: boolean;
  /** The runtime's declared/default execution backend. */
  protocol: 'terminal' | 'acp';
}

/** An operator-defined custom agent: the shell commands loom runs at each launch
 *  stage. Mirrors `custom_agents::CustomAgent`. Returned by `GET /api/agents`
 *  (the `custom` array) and round-tripped by the Agents settings editor. */
export interface CustomAgent {
  name: string;
  label: string;
  /** Shell run in the worktree before launch — e.g. installing status hooks. */
  setup: string;
  /** Fresh-session launch command; the goal is appended as an argument. */
  launch: string;
  /** Adopt/resume command (no goal). Blank reuses `launch`. */
  resume: string;
  /** Whether the agent fires weaver's lifecycle hooks. */
  reports_status: boolean;
  created_at: string;
  updated_at: string;
}

/** The editable fields the Agents editor sends to create/update a custom agent. */
export interface CustomAgentInput {
  name: string;
  label: string;
  setup: string;
  launch: string;
  resume: string;
  reports_status: boolean;
}

/** An issue belongs to a repo (`repo_root`). `claimed_branch` is the branch
 *  currently working it; `null` is the unclaimed repo backlog. `source_branch`
 *  records where it was created. */
export interface Issue {
  id: number;
  repo_root: string;
  github_repo: string | null;
  source_branch: string | null;
  claimed_branch: string | null;
  title: string;
  body: string;
  /** "open" or "closed". */
  status: string;
  github_issue: number | null;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
  /** Free-form `(key, value)` labels on the issue, rendered as quiet pills.
   *  Empty when the issue carries none. Unlike branch tags these never carry the
   *  loud `attention`/`triage` ladder. */
  tags: Tag[];
}

export interface IssueTagInput {
  key: string;
  value: string;
  note?: string;
  by?: string;
}

export type IssueAction =
  | { type: 'close' }
  | { type: 'reopen' }
  | { type: 'tag'; key: string; value: string; note?: string; by?: string }
  | { type: 'untag'; key: string }
  | { type: 'delete' };

export interface IssueActionsResult {
  issues: Issue[];
  deleted_ids: number[];
}

/** One invalid ID or precondition in an atomic issue action's error details. */
export interface IssueActionProblem {
  id: number;
  code: string;
  error: string;
}

// --- Artifacts -------------------------------------------------------------
// Named, versioned documents an agent (or the user) writes *to weaver*, not to
// the repo — designs, reports, the `plan`. Scoped like issues (branch-scoped or
// repo-shared), versioned by immutable snapshot, markdown-first. Mirrors
// weaver-api's artifact DTOs. See docs/artifacts.md.

/** An artifact envelope: identity, kind, title, scope, and its latest revision.
 *  `branch_id === null` is a repo-shared artifact; a branch-scoped name shadows
 *  a shared one in a session's listing. */
export interface ArtifactMeta {
  id: number;
  name: string;
  /** Defaults to `markdown` (GFM + mermaid); other kinds render as source. */
  kind: string;
  title: string;
  /** The branch that owns it, or `null` for a repo-shared artifact. */
  branch_id: string | null;
  /** The latest revision number. */
  rev: number;
  created_at: string;
  updated_at: string;
}

/** One revision of an artifact (metadata only — the picker lists these; content
 *  is fetched per-rev through the artifact GET with `?rev=`). */
export interface ArtifactVersion {
  rev: number;
  /** `agent` | `user` — who wrote this revision. */
  author: string;
  created_at: string;
}

/** The live status of one issue referenced from an artifact — what the renderer
 *  stamps into a `#N` chip. */
export interface IssueRefStatus {
  id: number;
  title: string;
  /** `open` | `closed`. */
  status: string;
  /** The branch working it; `null` is the unclaimed backlog. */
  claimed_branch: string | null;
}

/** The projected reference map an artifact's content names, keyed by issue id as
 *  a string. v1 projects issues; `issues` may be absent → default `{}`. */
export interface ArtifactRefs {
  issues: Record<string, IssueRefStatus>;
}

/** The full artifact view returned by the artifact GET/PUT: the envelope, the
 *  selected (default latest) revision's content, the version list for the
 *  picker, and the projected reference map. */
export interface ArtifactView {
  meta: ArtifactMeta;
  /** Raw content of the selected revision — rendered read-first, editable as source. */
  content: string;
  /** Every revision, newest first, for the version picker. */
  versions: ArtifactVersion[];
  /** References found in the content, resolved against the live ledger. */
  refs: ArtifactRefs;
}

/** Body for `PUT /api/sessions/{id}/artifacts/{name}`: a user edit that appends
 *  a new revision (`author: user`). `title`/`kind` update the envelope; omit
 *  them to keep the current values. */
export interface ArtifactWriteBody {
  content: string;
  title?: string;
  kind?: string;
  /** The revision the edit was based on, for conflict detection. */
  base_rev?: number;
}

// --- Discussion (margin comments) -------------------------------------------
// Google-Docs-style comment threads anchored to a quoted span of an artifact's
// rendered markdown. Mirrors weaver-api's discussion DTOs (`dto.rs`).

/** Where a thread's comment attaches: the quoted text plus enough surrounding
 *  context (`prefix`/`suffix`) for the frontend anchoring engine to relocate
 *  it in the rendered DOM after later edits. */
export interface Anchor {
  quote: string;
  prefix: string;
  suffix: string;
  /** Stable rendered block position captured with the quote selector. */
  block_index?: number | null;
}

/** One reply in a thread. */
export interface Comment {
  seq: number;
  /** `agent` | `user`. */
  author: string;
  body: string;
  created_at: string;
}

/** A discussion thread on an artifact span: its anchor, status, and comments
 *  (oldest first). */
export interface Thread {
  id: number;
  /** The artifact revision the anchor was taken from. */
  base_rev: number;
  anchor: Anchor;
  /** `open` | `resolved` | `orphaned` (its anchor no longer locates in the
   *  current rendered content). */
  status: string;
  created_at: string;
  resolved_at: string | null;
  comments: Comment[];
}

/** Body for `POST /api/sessions/{id}/artifacts/{name}/threads`: open a new
 *  thread anchored to a quoted span, seeded with its first comment. */
export interface NewThreadBody {
  base_rev: number;
  anchor: Anchor;
  body: string;
}

/** Body for `POST /api/sessions/{id}/artifacts/{name}/threads/{tid}/comments`:
 *  append a reply to an existing thread. */
export interface NewCommentBody {
  body: string;
}

// --- Staged reviews --------------------------------------------------------

export interface ReviewSubject {
  kind: 'artifact' | 'changes';
  /** Stable internal subject id. */
  id: string;
  /** Public round-trippable key; artifact reviews use the artifact name. */
  key: string;
  label: string;
  version: string;
  current_version: string;
}

export interface ReviewComment {
  id: number;
  subject_version: string;
  anchor_kind: 'text' | 'change';
  anchor: Anchor | ChangeAnchor;
  body: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface Review {
  id: number;
  session_id: string;
  subject: ReviewSubject;
  /** `draft` | `submitted`. */
  status: string;
  summary: string;
  /** Monotonic optimistic revision for every editable draft mutation. */
  draft_revision: number;
  /** Exact server-rendered payload that will be delivered on submit. */
  message: string;
  created_by: string;
  outdated: boolean;
  acknowledged_outdated: boolean;
  /** `draft` | `queued` | `delivering` | `retrying` | `delivered` | `failed`. */
  delivery_state: string;
  delivery_error: string | null;
  delivery_key: string;
  created_at: string;
  updated_at: string;
  submitted_at: string | null;
  comments: ReviewComment[];
  legacy: boolean;
}

export interface CreateReviewBody {
  subject_kind: 'artifact' | 'changes';
  subject_key: string;
  subject_version: string;
}

export interface AddReviewCommentBody {
  expected_revision: number;
  subject_version: string;
  anchor_kind: 'text' | 'change';
  anchor: Anchor | ChangeAnchor;
  body: string;
}

export interface UpdateReviewCommentBody {
  expected_revision: number;
  subject_version?: string;
  anchor_kind?: 'text' | 'change';
  anchor?: Anchor | ChangeAnchor;
  body?: string;
}

export interface UpdateReviewBody {
  expected_revision: number;
  summary?: string;
  subject_version?: string;
}

// --- Changes ---------------------------------------------------------------

export type ChangeFileStatus =
  'added' | 'modified' | 'deleted' | 'renamed' | 'copied' | 'type_changed' | 'untracked';
export type ChangeSource = 'committed' | 'staged' | 'unstaged' | 'untracked';
export type ChangeContent = 'text' | 'binary' | 'oversize' | 'unsupported';
export type ChangeLineKind = 'context' | 'addition' | 'deletion';
export type ChangeSide = 'old' | 'new';

export interface ChangePath {
  bytes: string;
  display: string;
}

export interface ChangeLine {
  kind: ChangeLineKind;
  old_line: number | null;
  new_line: number | null;
  text: string;
}

export interface ChangeHunk {
  header: string;
  lines: ChangeLine[];
  truncated: boolean;
}

export interface ChangeFile {
  status: ChangeFileStatus;
  path: ChangePath;
  old_path: ChangePath | null;
  sources: ChangeSource[];
  additions: number | null;
  deletions: number | null;
  content: ChangeContent;
  hunks: ChangeHunk[];
  truncated: boolean;
}

export type ChangeBase =
  | { state: 'available'; reference: string; oid: string }
  | {
      state: 'unavailable';
      reference: string;
      reason: 'unborn_head' | 'missing_base' | 'no_merge_base';
    };

export interface ChangeSet {
  version: string | null;
  base: ChangeBase;
  head_oid: string | null;
  totals: { files: number; additions: number; deletions: number; truncated: boolean };
  files: ChangeFile[];
  truncated: boolean;
  limits: {
    max_files: number;
    max_hunks_per_file: number;
    max_lines_per_file: number;
    max_total_lines: number;
    max_line_bytes: number;
  };
}

export interface ChangeAnchor {
  path: ChangePath;
  side: ChangeSide;
  start_line: number;
  end_line: number;
  hunk_header: string;
  context_before: string[];
  selected: string[];
  context_after: string[];
}

export interface RecentRepo {
  repo_root: string;
  last_used_at: string;
  active_branches: number;
}

/** A repository registered in the managed repo store (`/api/repos`). The
 *  slug→(remote, path) mapping doubles as the clone allowlist: only a registered
 *  repo may be cloned for a session. Mirrors loom's `repo::ManagedRepo`. */
export interface ManagedRepo {
  /** Canonical GitHub `owner/name`. */
  slug: string;
  /** The clone source URL. */
  remote_url: string;
  /** The managed on-disk clone path. */
  path: string;
  created_at: string;
}

/** Result of checking a proposed worktree fork point in a local repository. */
export interface RepoRevisionValidation {
  valid: boolean;
  repo_root: string;
  message: string | null;
}

/** One per-repo environment variable's metadata (`/api/repos/env`). Mirrors
 *  loom's `repo_env::RepoEnvVar`. The value is **write-only**: it is set via PUT
 *  but never returned (these hold per-repo secrets), so only the name and last
 *  change time appear here. */
export interface RepoEnvVar {
  name: string;
  updated_at: string;
}

/** Branch listing returned by `/api/repos/branches?cwd=...` — distinct from
 *  the tracked-branch model: this enumerates git branches in a repo on disk. */
export interface RepoBranch {
  name: string;
  worktree: string | null;
  current: boolean;
}

export interface WeaverEvent {
  id: number;
  branch_id: string;
  kind: string;
  data: Record<string, unknown>;
  created_at: string;
}

/** A file dropped into the worktree's `scratch/` directory. */
export interface ScratchFile {
  name: string;
  bytes: number;
}

/** Availability of the per-session embedded editor (code-server). Returned by
 *  `/api/sessions/{id}/ide-info`; the UI uses it to decide between mounting the
 *  editor iframe and showing a "not installed" note. */
export interface IdeInfo {
  /** The `ide.enabled` master switch. */
  enabled: boolean;
  /** Whether the `code-server` command is runnable on the loom host. */
  available: boolean;
  /** Idle-reap timeout, surfaced for the panel's info text. */
  idle_timeout_secs: number;
}

/** A watch's trigger — its subscription manifest, parsed. A scheduled
 *  trigger carries a `cron` (or `every`) cadence; a reactive one subscribes to
 *  one or more normalized trigger events via `on` (each `"name"` or
 *  `"name=level"`). `event`/`level` are the legacy single-event shape, still
 *  honoured. An optional `repo` pins it to one repository. Mirrors weaver-core's
 *  `Trigger`. */
export interface WatchTrigger {
  cron?: string;
  every?: string;
  /** The normalized trigger events this watch subscribes to, e.g.
   *  `["pr.merged", "session.exited=error"]`. */
  on?: string[];
  event?: string;
  level?: string;
  repo?: string;
}

/** The fleet query a round surveys, parsed. `attention` is `!ok` (anything not
 *  ok) or an exact level; `repo` scopes the survey to one repository. */
export interface WatchScope {
  attention?: string;
  repo?: string;
}

/** One watch: a periodic / triggered watch program over the fleet. The
 *  JSON-bearing fields (`trigger`, `scope`, `params`) arrive as parsed objects;
 *  `capabilities` is a real array. Mirrors `WatchView` in web.rs. */
export interface Watch {
  id: string;
  name: string;
  enabled: boolean;
  /** The event-match predicate: `{cron|every|event|level|repo}`. */
  trigger: WatchTrigger;
  /** The fleet query a round surveys: `{attention?, repo?}`. */
  scope: WatchScope;
  /** `builtin:<name>` for a stock program, or an absolute path under
   *  `~/.weaver/watches/` for a custom one. */
  program: string;
  /** Stock-program parameters, e.g. `{prompt}`. */
  params: Record<string, unknown>;
  /** The granted capability set (the intervention ladder). `observe` is
   *  implicit; the rest are explicit grants. */
  capabilities: string[];
  /** Automation-safe ACP profile used by agent judgements and warm sessions. */
  profile: string;
  model: string;
  effort: string;
  cooldown_secs: number;
  /** Warm mode (`params.warm`): the engine keeps one long-lived, fleet-hidden
   *  session for this watch so it has across-round memory. */
  warm: boolean;
  /** The id of that warm session once the engine has created it, else null. Its
   *  live terminal is reachable here (it is hidden from the fleet listing). */
  warm_session_id: string | null;
  last_run_at: string | null;
  next_run_at: string | null;
  /** A one-shot dynamic re-trigger a round armed for itself (a backoff recheck),
   *  or null. Distinct from `next_run_at` (the cron cadence). */
  wake_at: string | null;
  /** The program's lookaside state — its scratch memory carried across rounds
   *  (e.g. a backoff watcher's per-session attempt counts). `{}` when none. */
  state: Record<string, unknown>;
  /** The most recent round's outcome, or null if it has never run. */
  last_outcome: 'ok' | 'noop' | 'skipped' | 'error' | null;
  created_at: string;
  updated_at: string;
}

/** Create payload for `POST /api/watches`. */
export interface WatchCreateInput {
  name: string;
  trigger?: WatchTrigger;
  scope?: WatchScope;
  program?: string;
  params?: Record<string, unknown>;
  capabilities?: string[];
  profile?: string;
  model?: string;
  effort?: string;
  cooldown_secs?: number;
  enabled?: boolean;
}

/** Mutable fields accepted by `PATCH /api/watches/{id}`. */
export type WatchUpdateInput = Partial<Omit<WatchCreateInput, 'name'>>;

/** One action a round recorded — a mark, nudge, interrupt, or a stubbed
 *  "would do X" from a dry-run. The shape is loose (the engine writes free-form
 *  JSON); these are the fields the panel renders when present. */
export interface WatchAction {
  /** The session the action targeted, when it targets one. */
  session?: string;
  /** A performed action's verb (e.g. `mark`, `nudge`). */
  action?: string;
  /** A dry-run's stubbed verb — what it *would* have done. */
  would?: string;
  /** The triage level a `mark` stamped. */
  level?: string;
  /** A one-line reason / note. */
  note?: string;
  /** The message body of a nudge. */
  text?: string;
  [key: string]: unknown;
}

/** One round in a watch's history — the audit trail. `actions` is the
 *  array of marks / nudges / would-dos the round recorded; `stdout`/`stderr`/
 *  `exit_code`/`duration_ms` are the captured execution log — what the script
 *  printed and returned. Mirrors `WatchRunView` in web.rs. */
export interface WatchRun {
  id: number;
  trigger_reason: string;
  /** The normalized event that woke the round (`cron` / `manual` / e.g.
   *  `pr.merged`). */
  trigger_event: string;
  started_at: string;
  finished_at: string | null;
  outcome: 'ok' | 'noop' | 'skipped' | 'error' | string;
  summary: string;
  actions: WatchAction[];
  /** A tail of the script's standard output. */
  stdout: string;
  /** A tail of the script's standard error. */
  stderr: string;
  /** The interpreter's exit status, or null when it never spawned / timed out. */
  exit_code: number | null;
  /** Wall-clock the program ran, in milliseconds. */
  duration_ms: number | null;
}

/** The reply from `POST /api/watches/{id}/run`. */
export interface WatchRunResult {
  run_id: number;
  outcome: string;
  summary: string;
}

/** One program a watch can run, served by `GET /api/watches/programs`.
 *  Builtin programs are Python scripts that ship inside the loom binary; the
 *  embedded `source` is rendered read-only in the panel. `defaults` is the
 *  suggested starting config a create form prefills. Mirrors `ProgramView` in
 *  weaver-api. */
export interface ProgramView {
  /** The reference a watch's `program` field uses, e.g. `builtin:status`. */
  program: string;
  title: string;
  description: string;
  source: string;
  defaults: {
    trigger?: WatchTrigger;
    scope?: WatchScope;
    params?: Record<string, unknown>;
    capabilities?: string[];
  };
}

export type SettingKind = 'string' | 'text' | 'int' | 'bool' | 'enum';
export type SettingSource = 'default' | 'deployment' | 'runtime';

/** One configurable setting: its registry metadata plus its current value. */
export interface SettingView {
  key: string;
  label: string;
  description: string;
  kind: SettingKind;
  default: string;
  group: string;
  /** Allowed values for an `enum` setting, in display order; empty otherwise. */
  options: string[];
  value: string;
  /** Layer supplying the effective value: runtime > deployment > built-in. */
  source: SettingSource;
  /** Value declared by deployment IaC, when present. */
  deployment_value: string | null;
  is_default: boolean;
}

/** Canonical reply from both GET and PATCH /api/settings. */
export interface SettingsEnvelope {
  settings: SettingView[];
}

// --- Authentication --------------------------------------------------------

/** Which sign-in methods the login screen should offer. Mirrors weaver-api's
 *  `AuthMethods`. */
export interface AuthMethods {
  password: boolean;
  github: boolean;
}

/** Who the caller is + what the login screen needs (`GET /api/auth/me`).
 *  `authenticated: false` means show the login view. Mirrors `MeView`. */
export interface Me {
  authenticated: boolean;
  username: string | null;
  github_login: string | null;
  /** How they authenticated: `loopback` | `token` | `session` | null. */
  via: string | null;
  methods: AuthMethods;
}

/** One API token's non-secret metadata. Mirrors `TokenView`. */
export interface Token {
  id: string;
  name: string;
  prefix: string;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
}

/** The one-time create reply: the plaintext token plus its metadata (flattened).
 *  Mirrors `CreatedTokenView`. */
export interface CreatedToken extends Token {
  token: string;
}

/** One approved operator. Mirrors `UserView`. */
export interface User {
  username: string;
  github_login: string | null;
  has_password: boolean;
  created_at: string;
}

/** One readable environment variable on the default profile. Mirrors
 *  `agent_env::EnvVar`. */
export interface EnvVar {
  name: string;
  value: string;
  updated_at: string;
}

/** Named launch posture. Environment values are write-only; only metadata is returned. */
export interface Profile {
  name: string;
  description: string;
  agent_kind: string;
  model: string;
  effort: string;
  protocol: string;
  mode: string;
  class: 'interactive' | 'automation';
  strict: boolean;
  env_clear: boolean;
  ambient_allowlist: string[];
  idle_archive_secs: number | null;
  max_concurrent: number;
  turn_budget: number | null;
  prelude: 'weaver' | 'none';
  restricted: boolean;
  runtime_permissions: string[];
  mcp_access: McpAccess;
  lifetime: number;
  revision: number;
  created_at: string;
  updated_at: string;
  env: ProfileEnv[];
}

export interface ProfileEnv {
  name: string;
  source: 'literal' | 'gcp_secret';
  secret_ref: string | null;
  updated_at: string;
}

export interface ProfileEnvMutation {
  name: string;
  value?: string;
  secret_ref?: string;
}

export interface CloneProfileEnvironment {
  inherit: boolean;
  remove: string[];
  set: ProfileEnvMutation[];
}

export interface LaunchOverrides {
  agent?: string;
  model?: string;
  effort?: string;
  protocol?: string;
  mode?: string;
  class?: string;
}

export interface LaunchSelection {
  profile: string;
  overrides: LaunchOverrides;
}

export type LaunchSource =
  'profile' | 'agent_default' | 'origin_default' | 'policy_default' | 'launch_override';

export interface LaunchProvenance {
  agent: LaunchSource;
  model: LaunchSource;
  effort: LaunchSource;
  protocol: LaunchSource;
  mode: LaunchSource;
  class: LaunchSource;
  idle_archive_secs: LaunchSource;
  turn_budget: LaunchSource;
}

export interface LaunchCapacity {
  active: number;
  maximum: number | null;
  available: number | null;
  allowed: boolean;
}

export interface ResolvedLaunchPolicy {
  strict: boolean;
  restricted: boolean;
  env_clear: boolean;
  environment: ProfileEnv[];
  ambient_allowlist: string[];
  idle_archive_secs: number | null;
  turn_budget: number | null;
  prelude: string;
  runtime_permissions: string[];
  mcp_policy: SessionMcpPolicy;
}

export interface ResolvedLaunch {
  selection: LaunchSelection;
  profile_lifetime: number;
  profile_revision: number;
  resolver_revision: string;
  agent: string;
  model: string;
  effort: string;
  protocol: string;
  mode: string;
  class: 'interactive' | 'automation';
  locked_fields: string[];
  provenance: LaunchProvenance;
  capacity: LaunchCapacity;
  policy: ResolvedLaunchPolicy;
  valid: boolean;
  errors: string[];
}

export interface HandoffInput {
  /** Canonical clients send selection plus both preview revisions. */
  selection?: LaunchSelection;
  expected_profile_revision?: number;
  expected_resolver_revision?: string;
  /** Flattened compatibility selectors preserve the stamped session policy. */
  agent?: string;
  model?: string;
  effort?: string;
  mode?: string;
}

export interface CloneProfileInput {
  name: string;
  expected_profile_revision: number;
  expected_resolver_revision: string;
  overrides: LaunchOverrides;
  template?: ProfileInput;
  copy_environment: boolean;
  environment?: CloneProfileEnvironment;
}

export interface ScratchLimits {
  max_files: number;
  max_file_bytes: number;
  max_total_bytes: number;
  max_name_bytes: number;
}

/** Trusted MCP registry. Capability-set names are provider-neutral profile
 * policy; a runtime translates their exact tools to its own protocol. */
export interface McpRegistry {
  adapters: McpAdapter[];
  capability_sets: McpCapabilitySet[];
  custom_servers: CustomMcp[];
}

export interface McpAdapter {
  name: string;
  description: string;
  server_name: string;
}

export interface McpCapabilitySet {
  name: string;
  group: string;
  version: string;
  digest: string;
  description: string;
  adapter: string;
  tools: string[];
}

export interface McpAccess {
  mode: 'none' | 'all' | 'groups';
  groups: string[];
}

export interface SessionMcpPolicy {
  selection: McpAccess;
  capability_sets: McpCapabilitySet[];
  custom_servers: SessionCustomMcp[];
}

export interface SessionCustomMcp {
  identity: string;
  group: string;
  revision: number;
  digest: string;
  server_name: string;
  tools: string[];
}

export interface CustomMcp {
  identity: string;
  group: string;
  label: string;
  description: string;
  enabled: boolean;
  revision: number;
  digest: string;
  source: string;
  test_source: string;
  tools: string[];
  validation_state: 'ready' | 'failed';
  validation_message: string;
  created_at: string;
  updated_at: string;
}

export type CustomMcpInput = Pick<
  CustomMcp,
  'identity' | 'label' | 'description' | 'enabled' | 'source' | 'test_source'
>;

export type ProfileInput = Omit<
  Profile,
  'lifetime' | 'revision' | 'created_at' | 'updated_at' | 'env'
> & {
  expected_revision?: number;
};

/**
 * The GitHub App / sign-in config (secret withheld). Mirrors `GithubConfigView`.
 * A single GitHub App backs loom: its OAuth client powers sign-in
 * (`configured`/`client_id`), and the same App's id + private key power the
 * `@loom` trigger (`app_configured`/`app_id`).
 */
export interface GithubConfig {
  configured: boolean;
  client_id: string;
  callback_path: string;
  app_configured: boolean;
  app_id: string;
  app_slug: string;
}

/** What the Socket Mode supervisor is doing right now. Only `connected` can
 *  carry a mention to loom. */
export type SlackSocketState = 'idle' | 'connecting' | 'connected' | 'failed';

/** The live supervisor, as opposed to a fresh credential probe. `app_id` comes
 *  from the `hello` frame and names the Slack app the app-level token opened.
 *  `last_skip` is why the most recent trigger did not become a session — the
 *  integration's quietest failure. */
export interface SlackSocket {
  state: SlackSocketState;
  app_id: string | null;
  connected_at: string | null;
  last_error: string | null;
  last_event_at: string | null;
  events_received: number;
  sessions_launched: number;
  /** Mentions delivered into the session an automation run had routed that
   *  thread to, rather than launching a second one on the same thread. */
  followups_routed: number;
  last_skip: string | null;
  last_skip_at: string | null;
}

/** Who loom is in Slack. `token_kind` is `'user'` when `LOOM_SLACK_BOT_TOKEN`
 *  holds a person's token (`xoxp-…`) instead of the app's (`xoxb-…`): that
 *  connects and authenticates normally, then posts as that person and discards
 *  their mentions as loom's own. */
export interface SlackIdentity {
  user_id?: string;
  team_id?: string;
  token_kind?: 'bot' | 'user';
  error: string | null;
}

/** Who may launch a session: everyone in the installed workspace, or a list. */
export interface SlackAccess {
  mode: 'workspace' | 'listed';
  users: string[];
}

/** Every link in the Slack trigger path (`GET /api/slack/status`), reported
 *  link by link because a live socket is not the same as a working
 *  integration. */
export interface SlackStatus {
  enabled: boolean;
  app_token_set: boolean;
  bot_token_set: boolean;
  configured: boolean;
  identity: SlackIdentity | null;
  access: SlackAccess;
  default_repo: string;
  socket: SlackSocket;
}

// --- Conversation log (iris format) ----------------------------------------
// The normalized agent conversation served by `GET /sessions/{id}/conversation`.
// Mirrors `weaver_core::transcript::iris`: a list of role-tagged messages, each
// an ordered list of content blocks. The Conversation tab renders this.

/** A content block, discriminated by `kind` (serde `tag = "kind"`). */
export type IrisBlock =
  | { kind: 'text'; text: string }
  | { kind: 'thinking'; text: string }
  | { kind: 'tool_use'; name: string; input: unknown }
  | { kind: 'tool_result'; output: string; is_error: boolean }
  | { kind: 'image' };

/** One message: who said it, when, and its content blocks. */
export interface IrisMessage {
  role: 'user' | 'assistant' | 'context';
  timestamp?: string;
  blocks: IrisBlock[];
}

/** A whole normalized conversation. Mirrors `iris::Log`. */
export interface IrisLog {
  source: string;
  session_id?: string;
  model?: string;
  cwd?: string;
  messages: IrisMessage[];
}

/** One provider-neutral record from `GET /sessions/{id}/history[/search]`.
 * Optional fields are present only when the source transcript supplies them;
 * ACP tool activity, in particular, does not claim invocation arguments. */
export interface HistoryRecord {
  cursor: string;
  kind: 'message' | 'reasoning' | 'tool_call' | 'tool_result' | 'context' | 'event' | 'image';
  role?: string;
  content?: string;
  tool_name?: string;
  tool_input?: unknown;
  tool_status?: string;
  is_error?: boolean;
  event_name?: string;
  locations?: Array<{ path: string; line?: number }>;
  timestamp?: string;
}

/** A newest-tail page returned in chronological display order. */
export interface HistoryPage {
  source: string;
  records: HistoryRecord[];
  older_cursor?: string;
}

/** One captured server log line. Mirrors `loom::logs::LogLine`. */
export interface LogLine {
  seq: number;
  ts: string;
  level: string;
  target: string;
  message: string;
}

/** Build/runtime status of the server. Mirrors `loom::web::logview::ServerStatus`. */
export interface ServerStatus {
  version: string;
  build_revision: string;
  build_profile: string;
  image: string | null;
  pid: number;
  started_at: string;
}

export interface MigrationStream {
  stream: string;
  current: number;
  expected: number;
  applied: number;
  ready: boolean;
}

export interface DiagnosticSessionCount {
  status: string;
  class: string;
  profile: string;
  protocol: string;
  runner_pool: string;
  count: number;
}

export interface DiagnosticProfileCapacity {
  profile: string;
  revision: number;
  active: number;
  maximum: number | null;
  available: number | null;
}

export interface DiagnosticRunCount {
  status: string;
  source: string;
  service_tag: string;
  profile: string;
  count: number;
}

export interface DiagnosticRunFailure {
  source: string;
  profile: string;
  outcome: string | null;
  updated_at: string;
}

export interface DiagnosticProblemSummary {
  status: string;
  class: string;
  profile: string;
  protocol: string;
  runner_pool: string;
  count: number;
  latest_activity_at: string | null;
}

export interface DiagnosticFederation {
  name: string;
  provider: string;
  audience: string;
  service_tag: string;
  profiles: string[];
  created_at: string;
  updated_at: string;
}

/** Redacted admin operational snapshot from `GET /api/diagnostics`. */
export interface Diagnostics {
  sessions: DiagnosticSessionCount[];
  profiles: DiagnosticProfileCapacity[];
  automation_runs: {
    counts: DiagnosticRunCount[];
    stale_creating: number;
    recent_failures: DiagnosticRunFailure[];
  };
  problems: DiagnosticProblemSummary[];
  migrations: MigrationStream[];
  federations: DiagnosticFederation[];
}

/** One detached background task (a `@loom` webhook launch). Mirrors
 *  `loom::tasks::TaskRecord`. */
export interface TaskRecord {
  id: number;
  /** Coarse category, e.g. `github-trigger` or `github-unauthorized`. */
  kind: string;
  /** Human label, e.g. `owner/repo#123 (@user)`. */
  label: string;
  /** `running` | `done` | `error`. */
  state: string;
  /** Outcome detail: a session id, `forwarded…`, or an error message. */
  detail: string;
  started_at: string;
  finished_at: string | null;
}
