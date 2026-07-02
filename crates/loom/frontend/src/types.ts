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
  /** Who set it — `agent`, a watch/overlooker name, or `manual`. */
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
   *  shared team board. null for engine-created sessions (the concierge, warm
   *  overlooker sessions) and rows predating the column. A tracking/UX field, not
   *  a security boundary: the fleet stays co-owned by everyone authenticated. */
  created_by: string | null;
  /** The tracking issue opened for this session's task at launch (the handle
   *  the launcher follows). Only set on the create response. */
  tracking_issue: number | null;
  branch: Branch;
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
  supports_concierge: boolean;
  /** True for the builtin `claude`/`codex`; false for an operator-defined custom
   *  agent (which the UI may edit or delete). */
  builtin: boolean;
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

/** An overlooker's trigger — its subscription manifest, parsed. A scheduled
 *  trigger carries a `cron` (or `every`) cadence; a reactive one subscribes to
 *  one or more normalized trigger events via `on` (each `"name"` or
 *  `"name=level"`). `event`/`level` are the legacy single-event shape, still
 *  honoured. An optional `repo` pins it to one repository. Mirrors weaver-core's
 *  `Trigger`. */
export interface OverlookerTrigger {
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
export interface OverlookerScope {
  attention?: string;
  repo?: string;
}

/** One overlooker: a periodic / triggered watch program over the fleet. The
 *  JSON-bearing fields (`trigger`, `scope`, `params`) arrive as parsed objects;
 *  `capabilities` is a real array. Mirrors `OverlookerView` in web.rs. */
export interface Overlooker {
  id: string;
  name: string;
  enabled: boolean;
  /** The event-match predicate: `{cron|every|event|level|repo}`. */
  trigger: OverlookerTrigger;
  /** The fleet query a round surveys: `{attention?, repo?}`. */
  scope: OverlookerScope;
  /** `builtin:<name>` for a stock program, or an absolute path under
   *  `~/.weaver/overlookers/` for a custom one. */
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
   *  session for this overlooker so it has across-round memory. */
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
export interface OverlookerAction {
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

/** One round in an overlooker's history — the audit trail. `actions` is the
 *  array of marks / nudges / would-dos the round recorded; `stdout`/`stderr`/
 *  `exit_code`/`duration_ms` are the captured execution log — what the script
 *  printed and returned. Mirrors `OverlookerRunView` in web.rs. */
export interface OverlookerRun {
  id: number;
  trigger_reason: string;
  /** The normalized event that woke the round (`cron` / `manual` / e.g.
   *  `pr.merged`). */
  trigger_event: string;
  started_at: string;
  finished_at: string | null;
  outcome: 'ok' | 'noop' | 'skipped' | 'error' | string;
  summary: string;
  actions: OverlookerAction[];
  /** A tail of the script's standard output. */
  stdout: string;
  /** A tail of the script's standard error. */
  stderr: string;
  /** The interpreter's exit status, or null when it never spawned / timed out. */
  exit_code: number | null;
  /** Wall-clock the program ran, in milliseconds. */
  duration_ms: number | null;
}

/** The reply from `POST /api/overlookers/{id}/run`. */
export interface OverlookerRunResult {
  run_id: number;
  outcome: string;
  summary: string;
}

/** One program an overlooker can run, served by `GET /api/overlookers/programs`.
 *  Builtin programs are Python scripts that ship inside the loom binary; the
 *  embedded `source` is rendered read-only in the panel. `defaults` is the
 *  suggested starting config a create form prefills. Mirrors `ProgramView` in
 *  weaver-api. */
export interface ProgramView {
  /** The reference an overlooker's `program` field uses, e.g. `builtin:status`. */
  program: string;
  title: string;
  description: string;
  source: string;
  defaults: {
    trigger?: OverlookerTrigger;
    scope?: OverlookerScope;
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

/** One trusted GitHub owner — an account (org or user) whose App installations
 *  the inbound trigger will act for. Mirrors `owners::Owner`. */
export interface Owner {
  login: string;
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
