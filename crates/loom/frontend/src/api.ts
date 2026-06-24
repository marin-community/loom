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

import type { Issue, Session, ArtifactMeta, ArtifactView, ArtifactWriteBody, IdeInfo } from './types';

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

/** Availability of the session's embedded editor (code-server). */
export const ideInfo = (id: string) => get(`/sessions/${id}/ide-info`) as Promise<IdeInfo>;

/** Type a message into the session's agent pane and, by default, submit it with
 *  Enter to trigger a round (the same primitive the `loom` CLI's `send` wraps).
 *  Requires a live terminal — a torn-down or orphaned session 409s. */
export const sendMessage = (id: string, text: string, submit = true) =>
  post(`/sessions/${id}/send`, { text, submit });

// --- Chat (the fleet concierge) --------------------------------------------

/** Get-or-create the singleton fleet concierge and return its session view —
 *  what the Chat surface mounts its conversation against. 400s when no repo has
 *  been used yet (the concierge needs one to live in). */
export const getChat = () => get('/chat') as Promise<Session>;

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

/** The GitHub OAuth app config (secret withheld). */
export const getGithubConfig = () => get('/auth/github/config') as Promise<GithubConfig>;

/** Set the OAuth client id, and optionally the secret (omit to leave it). */
export const setGithubConfig = (clientId: string, clientSecret?: string) =>
  put('/auth/github/config', {
    client_id: clientId,
    ...(clientSecret !== undefined ? { client_secret: clientSecret } : {}),
  }) as Promise<GithubConfig>;
