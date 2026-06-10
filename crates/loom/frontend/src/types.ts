/** One (key, value) annotation on a branch. A status axis collapsed into data:
 *  the well-known *loud* keys are `attention` (authored by the agent) and
 *  `triage` (authored by an overlooker or `manual`), both on the
 *  `attention | blocked` ladder; every other key is a quiet, free-form pill.
 *  Absence is the calm state — there is no stored `ok`. Mirrors weaver-api's
 *  `TagView`. */
export interface Tag {
  key: string;
  value: string;
  /** One-line reason accompanying the tag. */
  note: string;
  /** Who set it — `agent`, an overlooker name, or `manual`. */
  set_by: string;
  /** When it was last set (ISO). The dashboard fades a triage mark stale once
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
   *  via `weaver set-status` (e.g. "Wired up routes; tests pass"). Shown even
   *  when the branch is calm. */
  description: string;
  /** Every tag on the branch: the agent's `attention`, an overlooker's
   *  `triage`, and any free-form key. Empty when the branch is calm and
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

/** A session is loom's view: one tmux + one running agent attached to a
 *  branch. Branch-level fields live under `branch`. */
export interface Session {
  id: string;
  status: string;
  work_dir: string;
  tmux_session: string;
  agent_kind: string;
  /** Model tier ('', 'haiku', 'sonnet', 'opus', 'fable') — spliced in as `--model`. */
  model: string;
  /** Reasoning effort ('', 'low', 'medium', 'high', 'xhigh', 'max') — `--effort`. */
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
  /** The tracking issue opened for this session's task at launch (the handle
   *  the launcher follows). Only set on the create response. */
  tracking_issue: number | null;
  branch: Branch;
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
  /** Link to a plan task (`"<slug>#T3"`) when materialized from a plan. */
  plan_task: string | null;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
}

/** One task in a plan. The structure (id, title, exec, value, deps) comes from
 *  the plan file; the status fields are PROJECTED from the linked issue at read
 *  time — never authored into the file. Returned within {@link PlanView}. */
export interface PlanTask {
  /** Stable id, `T<n>`. */
  id: string;
  title: string;
  /** 'session' | 'issue' (materialize into the ledger) | 'inline' | 'workflow'. */
  exec: string;
  /** Priority hint ('high' | 'med' | 'low' | …); drives sorting. */
  value: string;
  /** Ids of tasks this one depends on. */
  deps: string[];
  /** Linked issue id, or null when not (yet) materialized. */
  issue_id: number | null;
  /** Linked issue status ('open' | 'closed'), or null. */
  issue_status: string | null;
  /** Branch working the linked issue, or null (backlog / unmaterialized). */
  claimed_branch: string | null;
}

/** A structured project plan: design + task breakdown from a markdown file, with
 *  each task's status joined from the issue ledger. Returned by
 *  `/api/sessions/{id}/plan`. */
export interface PlanView {
  slug: string;
  /** Worktree-relative path, for saving edits via the file-write endpoint. */
  path: string;
  title: string;
  /** Frontmatter status ('draft' | 'active' | 'done' | …). */
  status: string;
  /** Raw markdown source — rendered read-first, edited in Monaco. */
  content: string;
  tasks: PlanTask[];
  /** Every plan slug in the repo, for the picker. */
  available: string[];
}

/** One reconcile action returned by `POST /api/sessions/{id}/plan/sync`. */
export interface PlanSyncAction {
  kind: 'create' | 'close' | 'update_title' | 'flag';
  task: string;
  title?: string;
  issue_id?: number;
  branch?: string;
  reason?: string;
}

/** The result of a plan reconcile: the delta, plus the in-flight flag count. */
export interface PlanSyncResult {
  applied: boolean;
  flags: number;
  actions: PlanSyncAction[];
}

export interface RecentRepo {
  repo_root: string;
  last_used_at: string;
  active_branches: number;
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

/** The worktree file tree: a flat list of repo-relative paths plus a
 *  `path → status` map of changes vs the chosen baseline. Assembled into a tree
 *  client-side. Returned by `/api/sessions/{id}/tree`; the optional `base`
 *  query param ("branch" | "uncommitted") selects the baseline, echoed back. */
export interface FileTree {
  files: string[];
  /** status is one of "added" | "modified" | "deleted" | "renamed" | "copied". */
  changed: Record<string, string>;
  /** The baseline these changes are measured against: "branch" | "uncommitted". */
  base: string;
}

/** A single file's content for the editor. For binary or oversized files the
 *  content is omitted and a flag is set instead. Returned by
 *  `/api/sessions/{id}/file`. */
export interface FileContent {
  path: string;
  content?: string;
  binary?: boolean;
  too_large?: boolean;
  bytes: number;
}

/** An overlooker's trigger — the event-match predicate, parsed. A scheduled
 *  trigger carries a `cron` (or `every`) cadence; a reactive one carries an
 *  `event` kind (and optional `level`). An optional `repo` pins it to one
 *  repository. Mirrors weaver-core's `Trigger`. */
export interface OverlookerTrigger {
  cron?: string;
  every?: string;
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
 *  array of marks / nudges / would-dos the round recorded. Mirrors
 *  `OverlookerRunView` in web.rs. */
export interface OverlookerRun {
  id: number;
  trigger_reason: string;
  started_at: string;
  finished_at: string | null;
  outcome: 'ok' | 'noop' | 'skipped' | 'error' | string;
  summary: string;
  actions: OverlookerAction[];
}

/** The reply from `POST /api/overlookers/{id}/run`. */
export interface OverlookerRunResult {
  run_id: number;
  outcome: string;
  summary: string;
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
