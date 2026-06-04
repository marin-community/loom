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
  description: string;
  repo_root: string;
  branch: string;
  base_branch: string;
  created_at: string;
  updated_at: string;
  open_issue_count: number;
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
  pending_prompt: string;
  github_repo: string | null;
  last_activity_at: string;
  summary_updated_at: string | null;
  created_at: string;
  updated_at: string;
  branch: Branch;
}

export interface Issue {
  id: number;
  branch_id: string;
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

export interface DiffStat {
  files_changed: number;
  insertions: number;
  deletions: number;
}

/** A file dropped into the worktree's `scratch/` directory. */
export interface ScratchFile {
  name: string;
  bytes: number;
}

/** The worktree file tree: a flat list of repo-relative paths plus a
 *  `path → status` map of changes vs the base branch. Assembled into a tree
 *  client-side. Returned by `/api/sessions/{id}/tree`. */
export interface FileTree {
  files: string[];
  /** status is one of "added" | "modified" | "deleted" | "renamed" | "copied". */
  changed: Record<string, string>;
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
