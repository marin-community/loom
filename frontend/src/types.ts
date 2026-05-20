export interface Workspace {
  id: string;
  name: string;
  title: string;
  goal: string;
  description: string;
  status: string;
  repo_root: string;
  work_dir: string;
  branch: string;
  base_branch: string;
  tmux_session: string;
  agent_kind: string;
  github_repo: string | null;
  github_issue: number | null;
  created_at: string;
  updated_at: string;
  last_activity_at: string;
  summary_updated_at: string | null;
  pending_prompt: string;
}

export interface RecentRepo {
  repo_root: string;
  last_used_at: string;
  active_workspaces: number;
}

export interface WeaverEvent {
  id: number;
  workspace_id: string;
  kind: string;
  data: Record<string, unknown>;
  created_at: string;
}

export interface DiffStat {
  files_changed: number;
  insertions: number;
  deletions: number;
}
