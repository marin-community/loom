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
  /** The agent's current-state message, set together with `attention` via
   *  `weaver set-status` (e.g. "Wired up routes; tests pass"). */
  description: string;
  /** Agent-declared attention level: 'ok' | 'attention' | 'blocked'. The
   *  "does this need me?" signal, set by the agent via `weaver set-status`. */
  attention: string;
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
  /** Model tier ('', 'haiku', 'sonnet', 'opus') — spliced in as `--model`. */
  model: string;
  /** Reasoning effort ('', 'low', 'medium', 'high', 'xhigh', 'max') — `--effort`. */
  effort: string;
  github_repo: string | null;
  last_activity_at: string;
  created_at: string;
  updated_at: string;
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
  created_at: string;
  updated_at: string;
  closed_at: string | null;
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

export type SettingKind = 'string' | 'int' | 'bool';

/** One configurable setting: its registry metadata plus its current value. */
export interface SettingView {
  key: string;
  label: string;
  description: string;
  kind: SettingKind;
  default: string;
  group: string;
  value: string;
  is_default: boolean;
}
