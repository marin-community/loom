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

/** A branch is the engine's view of "what the agent is working on": one
 *  `(repo_root, branch)` pair with a goal, a title, and a free-form
 *  description. Branches are owned by `weaver-core` and exist whether or not
 *  loom is running. */
export interface Branch {
  id: string;
  /** Short label: the branch name with the optional `weaver/` prefix stripped. */
  name: string;
  title: string;
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
  fetched_at: string;
}

/** A session is loom's view: one terminal + one running agent attached to a
 *  branch. Branch-level fields live under `branch`. */
export interface Session {
  id: string;
  status: string;
  work_dir: string;
  term_session: string;
  agent_kind: string;
  /** Model selector interpreted by the selected agent protocol. */
  model: string;
  /** Reasoning effort interpreted by the selected agent protocol. */
  effort: string;
  github_repo: string | null;
  last_activity_at: string;
  created_at: string;
  updated_at: string;
  /** Branch id of the session that launched this one (its parent in the session
   *  tree), or null for a top-level session. The dashboard groups the list into
   *  threads by it; a child whose parent is absent (archived, or never tracked)
   *  renders at the top level. Stamped on the session row at launch. */
  parent_id: string | null;
  /** The principal (username) that launched this session — attribution for the
   *  shared team board. null for engine-created warm watch sessions and rows
   *  predating the column. A tracking/UX field, not
   *  a security boundary: the fleet stays co-owned by everyone authenticated. */
  created_by: string | null;
  /** The tracking issue opened for this session's task at launch (the handle
   *  the launcher follows). Only set on the create response. */
  tracking_issue: number | null;
  /** Manual park override for the fleet list's resting shelf: `'parked'` pins the
   *  row to the shelf, `'active'` keeps it live even when idle, `null` = auto (the
   *  client shelves it once idle past the threshold). Set by dragging a row
   *  into/out of the Parked region. */
  park: 'parked' | 'active' | null;
  /** Manual fleet-list sort key, or `null` to follow the automatic
   *  urgency-then-recency order. Placed and untouched rows share one numeric axis
   *  so they interleave. Set by drag-reordering. */
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
  /** The latest context-window usage an ACP agent reported, or null. */
  usage: AcpUsage | null;
  /** The ACP modes the adapter offers, when the server exposes them. Absent today
   *  (SessionView carries only `current_mode`), so the mode chip falls back to the
   *  well-known claude/codex mode set — see `AcpConversation`. */
  available_modes?: string[];
  branch: Branch;
}

// ── ACP conversation surface ────────────────────────────────────────────────
// The chat journal + live SSE tail an `acp` session's Conversation renders from.
// These hand-mirror loom's `chat.rs` / `acp/mod.rs` serde shapes: the block
// contract (`GET /sessions/{id}/chat`) and the four `/chat/stream` SSE events.

/** Context-window usage `{used, size}` (tokens). */
export interface AcpUsage {
  used?: number | null;
  size?: number | null;
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
  live_turn: number | null;
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
export type ToolContent = ToolTextContent | ToolDiffContent;
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
  option_id: string;
  by: string;
  at: string;
}
export interface PermissionPayload {
  request_id: string;
  tool_call_id: string | null;
  title: string;
  options: PermissionOption[];
  outcome: PermissionOutcome | null;
}
export interface UsagePayload {
  used: number | null;
  size: number | null;
}
export interface TurnEndPayload {
  stop_reason: string;
}
export interface HandoffPayload {
  from: string;
  to: string;
  model: string;
  effort: string;
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
  stop_reason?: string;
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

export type SettingKind = 'string' | 'int' | 'bool' | 'enum';

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
  is_default: boolean;
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

/** One operator-managed agent environment variable. Exported into every
 *  interactive agent session loom launches. Mirrors `agent_env::EnvVar`. */
export interface EnvVar {
  name: string;
  value: string;
  updated_at: string;
}

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
  pid: number;
  started_at: string;
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
