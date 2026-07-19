// A 401 on any non-auth route means the session lapsed (or was never there);
// the app registers a handler that bounces to the login screen. Auth routes
// (`/auth/...`) are exempt: a bad-password 401 must surface in the form, not
// redirect.
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: () => void): void {
  onUnauthorized = fn;
}

async function request(path: string, opts: RequestInit = {}): Promise<unknown> {
  const res = await fetch('/api' + path, {
    headers: { 'content-type': 'application/json' },
    ...opts,
  });
  if (res.status === 401 && !path.startsWith('/auth/')) {
    onUnauthorized?.();
  }
  if (!res.ok) {
    let message = res.statusText;
    try {
      const body = await res.json();
      if (body && typeof body.error === 'string') message = body.error;
    } catch {
      /* keep statusText */
    }
    throw new Error(message);
  }
  if (res.status === 204) return null;
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

// Send a raw (not JSON-encoded) body — for scratch-file uploads. The server
// reads the bytes straight off the request body.
async function rawBody(method: string, path: string, body: BodyInit): Promise<unknown> {
  const res = await fetch('/api' + path, { method, body });
  if (!res.ok) {
    let message = res.statusText;
    try {
      const b = await res.json();
      if (b && typeof b.error === 'string') message = b.error;
    } catch {
      /* keep statusText */
    }
    throw new Error(message);
  }
  if (res.status === 204) return null;
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

export const upload = (path: string, body: BodyInit) => rawBody('POST', path, body);
export const get = (path: string) => request(path);
export const post = (path: string, body?: unknown) =>
  request(path, { method: 'POST', body: JSON.stringify(body ?? {}) });
export const put = (path: string, body?: unknown) =>
  request(path, { method: 'PUT', body: JSON.stringify(body ?? {}) });
export const patch = (path: string, body: unknown) =>
  request(path, { method: 'PATCH', body: JSON.stringify(body) });
export const del = (path: string) => request(path, { method: 'DELETE' });

// --- Issues ----------------------------------------------------------------

import type {
  Issue,
  Session,
  ArtifactMeta,
  ArtifactView,
  ArtifactWriteBody,
  IdeInfo,
  AgentMetadata,
  CustomAgent,
  CustomAgentInput,
  ManagedRepo,
  Thread,
  NewThreadBody,
  Comment,
  NewCommentBody,
  RepoEnvVar,
} from './types';

// --- Managed repos ---------------------------------------------------------

/** Every registered managed repo — the clone allowlist (`GET /api/repos`). */
export const listRepos = () => get('/repos') as Promise<ManagedRepo[]>;

/** Register a repo (a GitHub `owner/name` slug or clone URL) in the managed
 *  store / allowlist (`POST /api/repos`). Returns the stored mapping. */
export const registerRepo = (repo: string) => post('/repos', { repo }) as Promise<ManagedRepo>;

// --- Your GitHub token (per-user) ------------------------------------------

/** Whether the signed-in user has set a personal GitHub token. The token itself
 *  is write-only — set/cleared but never read back. */
export interface GithubTokenStatus {
  set: boolean;
  updated_at: string | null;
}

/** Whether you've set a personal GitHub token (`GET /api/auth/github-token`). */
export const getMyGithubToken = () => get('/auth/github-token') as Promise<GithubTokenStatus>;

/** Set/replace your personal GitHub token, injected as GH_TOKEN into the
 *  sessions you launch so your agents act as you (`PUT /api/auth/github-token`).
 *  Returns the refreshed status (never the token). */
export const setMyGithubToken = (token: string) =>
  put('/auth/github-token', { token }) as Promise<GithubTokenStatus>;

/** Clear your personal GitHub token; your sessions fall back to the shared
 *  ambient token (`DELETE /api/auth/github-token`). */
export const deleteMyGithubToken = () => del('/auth/github-token');

interface RepoEnvEnvelope {
  repo_root: string;
  env: RepoEnvVar[];
}

/** The per-repo env vars' metadata for a repo (`GET /api/repos/env`). Names and
 *  timestamps only — values are write-only and never returned. */
export const listRepoEnv = (repoRoot: string) =>
  get(`/repos/env?repo_root=${encodeURIComponent(repoRoot)}`).then(
    (r) => (r as RepoEnvEnvelope).env,
  );

/** Upsert one per-repo variable (`PUT /api/repos/env/{name}`); returns the
 *  refreshed metadata list (no values). */
export const setRepoEnv = (repoRoot: string, name: string, value: string) =>
  put(`/repos/env/${encodeURIComponent(name)}`, { repo_root: repoRoot, value }).then(
    (r) => (r as RepoEnvEnvelope).env,
  );

/** Delete one per-repo variable (`DELETE /api/repos/env/{name}`); returns the
 *  refreshed metadata list. */
export const deleteRepoEnv = (repoRoot: string, name: string) =>
  del(`/repos/env/${encodeURIComponent(name)}?repo_root=${encodeURIComponent(repoRoot)}`).then(
    (r) => (r as RepoEnvEnvelope).env,
  );

interface AgentsEnvelope {
  agents: AgentMetadata[];
  custom: CustomAgent[];
  default_agent: string;
}

export const listAgents = () => get('/agents') as Promise<AgentsEnvelope>;

interface CustomAgentsEnvelope {
  custom: CustomAgent[];
}

/** Define a new custom agent (`POST /api/agents/custom`). Returns the refreshed
 *  custom-agent list. */
export const createCustomAgent = (body: CustomAgentInput) =>
  (post('/agents/custom', body) as Promise<CustomAgentsEnvelope>).then((r) => r.custom);

/** Replace an existing custom agent's definition (`PUT /api/agents/custom/:name`;
 *  the name is immutable). Returns the refreshed list. */
export const updateCustomAgent = (name: string, body: CustomAgentInput) =>
  (put(`/agents/custom/${encodeURIComponent(name)}`, body) as Promise<CustomAgentsEnvelope>).then(
    (r) => r.custom,
  );

/** Delete a custom agent (`DELETE /api/agents/custom/:name`). Returns the
 *  refreshed list. */
export const deleteCustomAgent = (name: string) =>
  (del(`/agents/custom/${encodeURIComponent(name)}`) as Promise<CustomAgentsEnvelope>).then(
    (r) => r.custom,
  );

/** Every issue across every repo — the Issues pane's cross-repo board. Pass
 *  `all` to include closed issues. */
export const listIssues = (all = false) =>
  get(`/issues${all ? '?all=true' : ''}`) as Promise<Issue[]>;

/** Launch a new loom session that picks up (claims) an existing weaver issue:
 *  the issue's repo is the new session's cwd, and the backend seeds the branch's
 *  title/goal from the issue and stamps it as the tracking (claimed) issue.
 *  Returns the created session view, whose `id` deep-links to its detail page. */
export const launchSessionForIssue = (repoRoot: string, issueId: number) =>
  post('/sessions', { cwd: repoRoot, claim_issue: issueId }) as Promise<Session>;

/** Create an unclaimed repo-level backlog issue. Tags aren't part of the create
 *  body — apply them as follow-up `setIssueTag` upserts on the returned id. */
export const createRepoIssue = (repoRoot: string, title: string, body = '') =>
  post('/repos/issues', { repo_root: repoRoot, title, body }) as Promise<Issue>;

/** Patch an issue's title, body, and/or status ("open" | "closed"). */
export const patchIssue = (id: number, body: Partial<Pick<Issue, 'title' | 'body' | 'status'>>) =>
  patch(`/issues/${id}`, body) as Promise<Issue>;

/** Delete an issue outright. */
export const deleteIssue = (id: number) => del(`/issues/${id}`);

/** Set (upsert) a free-form label on an issue. */
export const setIssueTag = (id: number, key: string, value: string, note = '') =>
  put(`/issues/${id}/tags/${encodeURIComponent(key)}`, { value, note }) as Promise<Issue>;

/** Clear a label on an issue. */
export const clearIssueTag = (id: number, key: string) =>
  del(`/issues/${id}/tags/${encodeURIComponent(key)}`) as Promise<Issue>;

// --- Artifacts -------------------------------------------------------------

/** A session's artifacts: its branch-scoped documents plus the repo-shared ones
 *  (a branch-scoped name shadows a shared one). */
export const getArtifacts = (id: string) =>
  get(`/sessions/${id}/artifacts`) as Promise<ArtifactMeta[]>;

/** One artifact — content plus the projected ref map. `rev` selects a revision;
 *  omit it for the latest. */
export const getArtifact = (id: string, name: string, rev?: number) =>
  get(
    `/sessions/${id}/artifacts/${encodeURIComponent(name)}${rev != null ? `?rev=${rev}` : ''}`,
  ) as Promise<ArtifactView>;

/** Write a new revision of an artifact (a user edit, `author: user`), returning
 *  the refreshed view at the new latest revision. */
export const putArtifact = (id: string, name: string, body: ArtifactWriteBody) =>
  put(`/sessions/${id}/artifacts/${encodeURIComponent(name)}`, body) as Promise<ArtifactView>;

/** Delete an artifact and its whole revision history — the row the session sees
 *  for that name (its branch-scoped one, else the repo-shared). */
export const deleteArtifact = (id: string, name: string) =>
  del(`/sessions/${id}/artifacts/${encodeURIComponent(name)}`);

/** Availability of the session's embedded editor (code-server). */
export const ideInfo = (id: string) => get(`/sessions/${id}/ide-info`) as Promise<IdeInfo>;

// --- Discussion (margin comments) -------------------------------------------

/** Every thread on an artifact — open, resolved, and orphaned alike. */
export const listThreads = (id: string, name: string) =>
  get(`/sessions/${id}/artifacts/${encodeURIComponent(name)}/threads`) as Promise<Thread[]>;

/** Open a new thread anchored to a quoted span, seeded with its first comment. */
export const createThread = (id: string, name: string, body: NewThreadBody) =>
  post(`/sessions/${id}/artifacts/${encodeURIComponent(name)}/threads`, body) as Promise<Thread>;

/** Append a reply to an existing thread. */
export const addComment = (id: string, name: string, tid: number, body: NewCommentBody) =>
  post(
    `/sessions/${id}/artifacts/${encodeURIComponent(name)}/threads/${tid}/comments`,
    body,
  ) as Promise<Comment>;

/** Mark a thread resolved. */
export const resolveThread = (id: string, name: string, tid: number) =>
  post(
    `/sessions/${id}/artifacts/${encodeURIComponent(name)}/threads/${tid}/resolve`,
    {},
  ) as Promise<Thread>;

/** Type a message into the session's agent pane and, by default, submit it with
 *  Enter to trigger a round (the same primitive the `loom` CLI's `send` wraps).
 *  Requires a live terminal — a torn-down or orphaned session 409s. */
export const sendMessage = (id: string, text: string, submit = true) =>
  post(`/sessions/${id}/send`, { text, submit });

/** Replace the provider behind an idle ACP session while preserving its stable
 * loom session, worktree, branch, and canonical conversation journal. */
export const handoffSession = (
  id: string,
  body: { agent: string; model?: string; effort?: string; mode?: string },
) => post(`/sessions/${id}/handoff`, body) as Promise<Session>;

// --- ACP conversation (protocol='acp' sessions) ----------------------------

import type { AcpMetadata, ChatSnapshot, PromptAck } from './types';

/** The journaled conversation snapshot for an ACP session — the transcript a
 *  client paints before tailing `/chat/stream` (`GET /sessions/{id}/chat`). A
 *  terminal session 409s (it has no chat journal). */
export const getSessionChat = (id: string) => get(`/sessions/${id}/chat`) as Promise<ChatSnapshot>;

/** Send a user message to an ACP session. Returns 202 with
 *  `{ queued, steered, turn }`: a live turn is steered when the adapter supports
 *  it, with the durable next-turn queue retained as the fallback. */
export const promptSession = (id: string, text: string, by?: string) =>
  post(`/sessions/${id}/prompt`, { text, by }) as Promise<PromptAck>;

/** Interrupt the in-flight turn: `session/cancel` for an ACP session, an Escape
 *  keystroke for a terminal one. */
export const interruptSession = (id: string) =>
  post(`/sessions/${id}/interrupt`) as Promise<{ interrupted: boolean }>;

/** Answer a pending permission request (`{option_id}`). 404 for an unknown id,
 *  409 when it was already resolved. */
export const answerPermission = (id: string, requestId: string, optionId: string, by?: string) =>
  post(`/sessions/${id}/permissions/${encodeURIComponent(requestId)}`, {
    option_id: optionId,
    by,
  }) as Promise<{ resolved: boolean; option_id: string }>;

/** Change an ACP session's mode (`session/set_mode`). */
export const setSessionMode = (id: string, modeId: string, by?: string) =>
  put(`/sessions/${id}/mode`, { mode_id: modeId, by }) as Promise<{ mode_id: string }>;

/** Change an agent-owned ACP session configuration selector (model, reasoning
 * effort, or an adapter-specific option). */
export const setSessionConfigOption = (id: string, configId: string, value: string | boolean) =>
  put(`/sessions/${id}/config/${encodeURIComponent(configId)}`, { value }) as Promise<{
    config_id: string;
    value: string | boolean;
    metadata: AcpMetadata;
  }>;

/** Set a session's park override — the fleet list's resting shelf. `'parked'`
 *  pins it to the shelf, `'active'` keeps it live even when idle, `'auto'` clears
 *  the override back to idle-driven. */
export const setSessionPark = (id: string, park: 'parked' | 'active' | 'auto') =>
  patch(`/sessions/${id}`, { park }) as Promise<Session>;

/** Set a session's manual sort key (the drag-reorder midpoint). */
export const setSessionOrder = (id: string, sort_order: number) =>
  patch(`/sessions/${id}`, { sort_order }) as Promise<Session>;

// --- Chat (the fleet concierge) --------------------------------------------

/** Get-or-create the singleton fleet concierge and return its session view —
 *  what the Chat surface mounts its conversation against. 400s when no repo has
 *  been used yet (the concierge needs one to live in). */
export const getChat = () => get('/chat') as Promise<Session>;

/** Start a clean conversation: archive the current concierge (capturing its
 *  transcript) and launch a fresh one. Returns the new session view. */
export const resetChat = () => post('/chat/reset') as Promise<Session>;

// --- Agent environment variables -------------------------------------------

import type { EnvVar } from './types';

interface EnvEnvelope {
  env: EnvVar[];
}

/** The operator-managed env vars exported into every agent session. */
export const listEnv = () => get('/env').then((r) => (r as EnvEnvelope).env);

/** Upsert a variable by name; returns the refreshed list. */
export const setEnv = (name: string, value: string) =>
  put(`/env/${encodeURIComponent(name)}`, { value }).then((r) => (r as EnvEnvelope).env);

/** Delete a variable by name; returns the refreshed list. */
export const deleteEnv = (name: string) =>
  del(`/env/${encodeURIComponent(name)}`).then((r) => (r as EnvEnvelope).env);

/** Reset the operator scratch shell — kill it and spawn a fresh login shell. */
export const restartShell = () => post('/shell/restart');

// --- Authentication --------------------------------------------------------

import type { Me, Token, CreatedToken, User, GithubConfig } from './types';

/** Who the caller is + which sign-in methods to offer. Never 401s. */
export const getMe = () => get('/auth/me') as Promise<Me>;

/** Username/password login; sets the session cookie on success. */
export const login = (username: string, password: string) =>
  post('/auth/login', { username, password });

/** Drop the session and clear the cookie. */
export const logout = () => post('/auth/logout');

/** Begin GitHub OAuth — a full-page navigation (the server 302s to GitHub). */
export const githubLoginUrl = '/api/auth/github/login';

/** The user-managed API tokens. */
export const listTokens = () => get('/auth/tokens') as Promise<Token[]>;

/** Mint a token; the plaintext is in the reply once and never again. */
export const createToken = (name: string, expiresInDays?: number | null) =>
  post('/auth/tokens', { name, expires_in_days: expiresInDays ?? null }) as Promise<CreatedToken>;

/** Revoke a token by id. */
export const revokeToken = (id: string) => del(`/auth/tokens/${encodeURIComponent(id)}`);

/** Set/change the caller's own password. */
export const setPassword = (newPassword: string) =>
  post('/auth/password', { new_password: newPassword });

/** The approved-operator allowlist. */
export const listUsers = () => get('/auth/users') as Promise<User[]>;

/** Approve a new operator (GitHub login and/or password). */
export const addUser = (username: string, githubLogin?: string, password?: string) =>
  post('/auth/users', {
    username,
    github_login: githubLogin || null,
    password: password || null,
  }) as Promise<User>;

/** Remove an approved operator. */
export const removeUser = (username: string) => del(`/auth/users/${encodeURIComponent(username)}`);

/** The GitHub App / sign-in config (secret withheld). */
export const getGithubConfig = () => get('/auth/github/config') as Promise<GithubConfig>;

/** Set the sign-in OAuth client id, and optionally the secret (omit to leave it). */
export const setGithubConfig = (clientId: string, clientSecret?: string) =>
  put('/auth/github/config', {
    client_id: clientId,
    ...(clientSecret !== undefined ? { client_secret: clientSecret } : {}),
  }) as Promise<GithubConfig>;

// --- Server logs / debug ---------------------------------------------------

/** A snapshot of the most recent server log lines (oldest first). The live tail
 *  is an EventSource on `/api/logs/stream`, opened directly by the Logs panel. */
export const getLogs = (limit = 500) =>
  get(`/logs?limit=${limit}`) as Promise<import('./types').LogLine[]>;

/** Build version, pid, and start time of the running server. */
export const getServerStatus = () => get('/status') as Promise<import('./types').ServerStatus>;

/** Recent detached background tasks (the `@loom` webhook launches that run off the
 *  request), newest first. Operator-only, like the log endpoints. */
export const getTasks = () => get('/tasks') as Promise<import('./types').TaskRecord[]>;
