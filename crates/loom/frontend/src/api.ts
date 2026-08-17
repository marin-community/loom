// A 401 on any non-auth route means the session lapsed (or was never there);
// the app registers a handler that bounces to the login screen. Auth routes
// (`/auth/...`) are exempt: a bad-password 401 must surface in the form, not
// redirect.
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: () => void): void {
  onUnauthorized = fn;
}

/** HTTP failure with the server's structured body preserved. Create failures
 * use `body.session_id` to route the browser to the durable error session. */
export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly body: Record<string, unknown>,
  ) {
    super(message);
    Object.setPrototypeOf(this, new.target.prototype);
    this.name = 'ApiError';
  }
}

async function responseError(res: Response): Promise<ApiError> {
  let message = res.statusText;
  let body: Record<string, unknown> = {};
  try {
    const parsed: unknown = await res.json();
    if (parsed && typeof parsed === 'object') body = parsed as Record<string, unknown>;
    if (typeof body.error === 'string') message = body.error;
  } catch {
    /* keep statusText */
  }
  return new ApiError(message, res.status, body);
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
    throw await responseError(res);
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
    throw await responseError(res);
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
export const destroy = (path: string, body: unknown) =>
  request(path, { method: 'DELETE', body: JSON.stringify(body) });

/** Upload one prompt/reference attachment into the session-owned scratch dir. */
export const uploadSessionScratch = (id: string, file: File) =>
  upload(`/sessions/${id}/scratch?name=${encodeURIComponent(file.name)}`, file) as Promise<
    ScratchFile & { path: string }
  >;

/** Server-owned attachment limits shared by launch staging and live Scratch. */
export const getScratchLimits = () => get('/scratch/limits') as Promise<ScratchLimits>;

// --- Sessions ----------------------------------------------------------------

/** Compact fleet inventory/search. Full session context is fetched from the
 * item endpoint only when a row or session page discloses it. */
export const listSessionSummaries = (
  opts: {
    archived?: boolean;
    archivedOnly?: boolean;
    automation?: boolean;
    query?: string;
    status?: SessionSearchOptions['status'];
    attention?: SessionSearchOptions['attention'];
    creator?: SessionSearchOptions['creator'];
  } = {},
  signal?: AbortSignal,
) => {
  const params = new URLSearchParams();
  if (opts.archived) params.set('archived', 'true');
  if (opts.archivedOnly) params.set('archived_only', 'true');
  if (opts.automation !== undefined) params.set('automation', String(opts.automation));
  if (opts.query) params.set('q', opts.query);
  if (opts.status) params.set('status', opts.status);
  if (opts.attention) params.set('attention', opts.attention);
  if (opts.creator) params.set('creator', opts.creator);
  const qs = params.toString();
  return request(`/sessions/summary${qs ? `?${qs}` : ''}`, { signal }) as Promise<SessionSummary[]>;
};

export const getSession = (id: string) =>
  get(`/sessions/${encodeURIComponent(id)}`) as Promise<Session>;

/** Durable automation launch reservations, including failures that never
 *  produced a usable session (`GET /api/runs`). */
export const listRuns = () => get('/runs') as Promise<AutomationRun[]>;
export const archiveSession = (id: string) => post(`/sessions/${encodeURIComponent(id)}/archive`);
export const removeSession = (id: string) => del(`/sessions/${encodeURIComponent(id)}`);
export const clearSessionTag = (id: string, key: string) =>
  del(`/sessions/${encodeURIComponent(id)}/tags/${encodeURIComponent(key)}`);
export const regenerateSessionTitle = (id: string) =>
  post(`/sessions/${encodeURIComponent(id)}/title/regenerate`) as Promise<Session>;
export const setSessionTitleGeneration = (id: string, enabled: boolean) =>
  put(`/sessions/${encodeURIComponent(id)}/title-generation`, { enabled }) as Promise<Session>;
export const getResumptionCue = (id: string) =>
  get(`/sessions/${encodeURIComponent(id)}/resumption-cue`) as Promise<
    import('./types').ResumptionCue
  >;
export const ensureResumptionCue = (id: string, force = false) =>
  post(`/sessions/${encodeURIComponent(id)}/resumption-cue`, { force }) as Promise<
    import('./types').ResumptionCue
  >;

// --- Session layout ---------------------------------------------------------

export const getSessionLayout = () => get('/session-layout') as Promise<SessionLayout>;
export const createSessionSpace = (name: string, expectedRevision: number) =>
  post('/session-layout/spaces', {
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const updateSessionSpace = (id: string, name: string, expectedRevision: number) =>
  patch(`/session-layout/spaces/${encodeURIComponent(id)}`, {
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const deleteSessionSpace = (
  id: string,
  destinationGroupId: string | null,
  expectedRevision: number,
) =>
  destroy(`/session-layout/spaces/${encodeURIComponent(id)}`, {
    destination_group_id: destinationGroupId,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const createSessionGroup = (spaceId: string, name: string, expectedRevision: number) =>
  post('/session-layout/groups', {
    space_id: spaceId,
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const updateSessionGroup = (id: string, name: string, expectedRevision: number) =>
  patch(`/session-layout/groups/${encodeURIComponent(id)}`, {
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const deleteSessionGroup = (
  id: string,
  destinationGroupId: string | null,
  expectedRevision: number,
) =>
  destroy(`/session-layout/groups/${encodeURIComponent(id)}`, {
    destination_group_id: destinationGroupId,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const reorderSessionLayout = (body: {
  kind: SessionLayoutItemKind;
  id: string;
  before_id?: string | null;
  destination_space_id?: string | null;
  expected_revision: number;
}) => post('/session-layout/reorder', body) as Promise<SessionLayout>;
export const moveSessions = (body: {
  session_ids: string[];
  destination_group_id: string;
  before_session_id?: string | null;
  expected_revision: number;
}) => post('/session-layout/moves', body) as Promise<SessionLayout>;
export const restoreSessionGroups = (body: {
  groups: SessionGroupOrder[];
  expected_revision: number;
}) => post('/session-layout/restores', body) as Promise<SessionLayout>;
export const setSessionGroupPreference = (groupId: string, collapsed: boolean) =>
  put(`/session-layout/groups/${encodeURIComponent(groupId)}/preference`, {
    collapsed,
  }) as Promise<SessionLayout>;

// --- Issues ----------------------------------------------------------------

import type {
  Issue,
  IssueAction,
  IssueActionsResult,
  IssueTagInput,
  Session,
  SessionSummary,
  AutomationRun,
  SessionGroupOrder,
  SessionLayoutItemKind,
  SessionLayout,
  SessionSearchOptions,
  ArtifactMeta,
  ArtifactView,
  ArtifactWriteBody,
  Review,
  ReviewComment,
  CreateReviewBody,
  AddReviewCommentBody,
  UpdateReviewCommentBody,
  UpdateReviewBody,
  ChangeSet,
  IdeInfo,
  AgentMetadata,
  CustomAgent,
  CustomAgentInput,
  ManagedRepo,
  RepoRevisionValidation,
  Thread,
  NewThreadBody,
  Comment,
  NewCommentBody,
  RepoEnvVar,
  ScratchFile,
  ScratchLimits,
  Watch,
  WatchCreateInput,
  WatchRun,
  WatchRunResult,
  WatchUpdateInput,
  ProgramView,
  Channel,
  ChannelMessage,
  ChannelSubscription,
} from './types';

// --- Channels --------------------------------------------------------------

export const listChannels = (archived = false) =>
  get(`/channels?archived=${archived}`) as Promise<Channel[]>;

export const getChannel = (id: string) =>
  get(`/channels/${encodeURIComponent(id)}`) as Promise<Channel>;

export const createChannel = (name: string, topic: string, repoRoot: string) =>
  post('/channels', { name, topic, repo_root: repoRoot }) as Promise<Channel>;

export const listChannelMessages = (id: string, after = 0) =>
  get(`/channels/${encodeURIComponent(id)}/messages?after=${Math.max(0, after)}`) as Promise<
    ChannelMessage[]
  >;

export const sendChannelMessage = (
  id: string,
  body: string,
  kind: ChannelMessage['kind'] = 'message',
  urgency: ChannelMessage['urgency'] = 'normal',
) =>
  post(`/channels/${encodeURIComponent(id)}/messages`, {
    body,
    kind,
    urgency,
    payload: {},
  }) as Promise<ChannelMessage>;

export const markChannelRead = (id: string, seq?: number) =>
  put(`/channels/${encodeURIComponent(id)}/read-marker`, {
    seq,
  }) as Promise<ChannelSubscription>;

// --- Managed repos ---------------------------------------------------------

/** Every registered managed repo — the clone allowlist (`GET /api/repos`). */
export const listRepos = () => get('/repos') as Promise<ManagedRepo[]>;

/** Register a repo (a GitHub `owner/name` slug or clone URL) in the managed
 *  store / allowlist (`POST /api/repos`). Returns the stored mapping. */
export const registerRepo = (repo: string) => post('/repos', { repo }) as Promise<ManagedRepo>;

/** Check that a proposed worktree base resolves to a commit in a local repo. */
export const validateRepoRevision = (cwd: string, revision: string) => {
  const params = new URLSearchParams({ cwd, revision });
  return get(`/repos/revisions/validate?${params}`) as Promise<RepoRevisionValidation>;
};

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

/** Clear your personal GitHub token; new interactive sessions retain any
 *  credential supplied by their selected profile (`DELETE /api/auth/github-token`). */
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
 *  `all` to include closed issues, `automation` to include issues claimed by an
 *  automation-class session (the issue board retains this policy filter even
 *  though the session fleet is unified). */
export const listIssues = (opts: { all?: boolean; automation?: boolean } = {}) => {
  const params = new URLSearchParams();
  if (opts.all) params.set('all', 'true');
  if (opts.automation) params.set('automation', 'true');
  const qs = params.toString();
  return get(`/issues${qs ? `?${qs}` : ''}`) as Promise<Issue[]>;
};

/** Launch a new loom session that picks up (claims) an existing weaver issue:
 *  the issue's repo is the new session's cwd, and the backend seeds the branch's
 *  title/goal from the issue and stamps it as the tracking (claimed) issue.
 *  Returns the created session view, whose `id` deep-links to its detail page. */
export const launchSessionForIssue = (repoRoot: string, issueId: number) =>
  post('/sessions', { cwd: repoRoot, claim_issue: issueId }) as Promise<Session>;

/** Create an unclaimed repo-level backlog issue and its initial tags atomically. */
export const createRepoIssue = (
  repoRoot: string,
  title: string,
  body = '',
  tags: IssueTagInput[] = [],
) => post('/repos/issues', { repo_root: repoRoot, title, body, tags }) as Promise<Issue>;

/** Patch an issue's editable fields. Blank `github` unlinks it;
 *  `claimed_branch: null` returns it to the unclaimed backlog. */
export const patchIssue = (
  id: number,
  body: Partial<Pick<Issue, 'title' | 'body' | 'status'>> & {
    github?: string;
    claimed_branch?: null;
  },
) => patch(`/issues/${id}`, body) as Promise<Issue>;

/** Pin or clear a session's PR association. */
export const setSessionGithub = (id: string, prNumber: number) =>
  put(`/sessions/${id}/github`, { pr_number: prNumber }) as Promise<Session>;
export const clearSessionGithub = (id: string) => del(`/sessions/${id}/github`) as Promise<Session>;

/** Delete an issue outright. */
export const deleteIssue = (id: number) => del(`/issues/${id}`);

/** Apply one action atomically to every issue id. */
export const issueActions = (ids: number[], action: IssueAction) =>
  post('/issues/actions', { ids, action }) as Promise<IssueActionsResult>;

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

// --- Staged reviews --------------------------------------------------------

export const listArtifactReviews = (id: string, name: string) =>
  get(
    `/sessions/${id}/reviews?subject_kind=artifact&subject_key=${encodeURIComponent(name)}`,
  ) as Promise<Review[]>;

export const getChanges = (id: string) => get(`/sessions/${id}/changes`) as Promise<ChangeSet>;

export const listChangesReviews = (id: string) =>
  get(`/sessions/${id}/reviews?subject_kind=changes&subject_key=changes`) as Promise<Review[]>;

export const createReview = (id: string, body: CreateReviewBody) =>
  post(`/sessions/${id}/reviews`, body) as Promise<Review>;

export const addReviewComment = (reviewId: number, body: AddReviewCommentBody) =>
  post(`/reviews/${reviewId}/comments`, body) as Promise<Review>;

export const updateReviewComment = (
  reviewId: number,
  commentId: number,
  body: UpdateReviewCommentBody,
) => patch(`/reviews/${reviewId}/comments/${commentId}`, body) as Promise<Review>;

export const updateReview = (reviewId: number, body: UpdateReviewBody) =>
  patch(`/reviews/${reviewId}`, body) as Promise<Review>;

export const deleteReviewComment = (
  reviewId: number,
  commentId: number,
  expectedRevision: number,
) =>
  destroy(`/reviews/${reviewId}/comments/${commentId}`, {
    expected_revision: expectedRevision,
  }) as Promise<Review>;

export const discardReview = (reviewId: number, expectedRevision: number) =>
  destroy(`/reviews/${reviewId}`, { expected_revision: expectedRevision });

export const submitReview = (
  reviewId: number,
  body: { expected_revision: number; acknowledge_outdated: boolean },
) => post(`/reviews/${reviewId}/submit`, body) as Promise<Review>;

export const retargetReviewToCurrent = (reviewId: number, expectedRevision: number) =>
  post(`/reviews/${reviewId}/retarget-current`, {
    expected_revision: expectedRevision,
  }) as Promise<Review>;

export const retryReviewDelivery = (reviewId: number) =>
  post(`/reviews/${reviewId}/retry-delivery`, {}) as Promise<Review>;

export const setReviewCommentResolution = (
  reviewId: number,
  commentId: number,
  resolved: boolean,
) =>
  post(`/reviews/${reviewId}/comments/${commentId}/resolve`, {
    resolved,
  }) as Promise<ReviewComment>;

/** Type a message into the session's agent pane and, by default, submit it with
 *  Enter to trigger a round (the same primitive the `loom` CLI's `send` wraps).
 *  Requires a live terminal — a torn-down or orphaned session 409s. */
export const sendMessage = (id: string, text: string, submit = true) =>
  post(`/sessions/${id}/send`, { text, submit });

/** Replace the provider behind an idle ACP session while preserving its stable
 * loom session, worktree, branch, and canonical conversation journal. */
export const handoffSession = (id: string, body: import('./types').HandoffInput) =>
  post(`/sessions/${id}/handoff`, body) as Promise<Session>;
/** Recover a session: restart a failed live ACP runtime while preserving its
 * worktree/journal, or rebuild and resume an archived session. */
export const recoverSession = (id: string) => post(`/sessions/${id}/recover`) as Promise<Session>;
/** Resolve a profile-first handoff against an existing session's class and
 * capacity slot before sending its optimistic revisions. */
export const resolveSessionHandoff = (id: string, selection: LaunchSelection) =>
  post(`/sessions/${id}/handoff/resolve`, { selection }) as Promise<ResolvedLaunch>;

// --- ACP conversation (protocol='acp' sessions) ----------------------------

import type { AcpMetadata, ChatSnapshot, PromptAck } from './types';

/** A newest-first page of the journaled ACP conversation. The response is in
 * display order and carries an exclusive cursor for the next older page. */
export const getSessionChat = (id: string, before?: { turn: number; seq: number } | null) => {
  const query = before
    ? `?before_turn=${encodeURIComponent(before.turn)}&before_seq=${encodeURIComponent(before.seq)}`
    : '';
  return get(`/sessions/${id}/chat${query}`) as Promise<ChatSnapshot>;
};

/** Send a user message to an ACP session now. A receptive live turn is steered;
 *  a turn blocked behind a tool or permission is stopped and replaced. */
export const promptSession = (id: string, text: string, by?: string, files: string[] = []) =>
  post(`/sessions/${id}/prompt`, {
    text,
    by,
    send_now: true,
    files,
  }) as Promise<PromptAck>;

/** Send all durable next-turn feedback now, stopping a live turn first. */
export const forceQueuedSession = (id: string, by?: string) =>
  post(`/sessions/${id}/prompt`, {
    text: '',
    by,
    force_queued: true,
    files: [],
  }) as Promise<PromptAck>;

/** Atomically pull unseen next-turn feedback out of the server queue so it can
 * be edited in the composer. A 409 means the current ACP state has no queue
 * available to retract. */
export const retractQueuedSession = (id: string) =>
  del(`/sessions/${id}/prompt`) as Promise<{ text: string }>;

/** Worktree-backed completion for `@file` mentions in the ACP composer. */
export const listSessionFiles = (id: string, query: string) =>
  get(`/sessions/${id}/files?q=${encodeURIComponent(query)}`) as Promise<{ files: string[] }>;

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

// --- Launch profiles -------------------------------------------------------

import type {
  CloneProfileInput,
  CustomMcp,
  CustomMcpInput,
  LaunchSelection,
  Profile,
  ProfileInput,
  ResolvedLaunch,
} from './types';

export const listProfiles = () => get('/profiles') as Promise<Profile[]>;
export const resolveSessionLaunch = (selection: LaunchSelection) =>
  post('/session-launches/resolve', { selection }) as Promise<ResolvedLaunch>;
export const getMcpRegistry = () => get('/mcps') as Promise<import('./types').McpRegistry>;
export const createCustomMcp = (input: CustomMcpInput) =>
  post('/mcps/custom', input) as Promise<CustomMcp>;
export const updateCustomMcp = (identity: string, input: CustomMcpInput) =>
  put(`/mcps/custom/${identity.replace(/^\/+/, '')}`, input) as Promise<CustomMcp>;
export const deleteCustomMcp = (identity: string) =>
  del(`/mcps/custom/${identity.replace(/^\/+/, '')}`);
export const createProfile = (profile: ProfileInput) =>
  post('/profiles', profile) as Promise<Profile>;
export const updateProfile = (name: string, profile: ProfileInput) =>
  put(`/profiles/${encodeURIComponent(name)}`, profile) as Promise<Profile>;
export const cloneProfile = (source: string, input: CloneProfileInput) =>
  post(`/profiles/${encodeURIComponent(source)}/clone`, input) as Promise<Profile>;
export const deleteProfile = (name: string) => del(`/profiles/${encodeURIComponent(name)}`);
export const setProfileEnv = (profile: string, name: string, value: string) =>
  put(`/profiles/${encodeURIComponent(profile)}/env/${encodeURIComponent(name)}`, {
    value,
  }) as Promise<Profile>;
export const deleteProfileEnv = (profile: string, name: string) =>
  del(
    `/profiles/${encodeURIComponent(profile)}/env/${encodeURIComponent(name)}`,
  ) as Promise<Profile>;

/** Reset the operator scratch shell — kill it and spawn a fresh login shell. */
export const restartShell = () => post('/shell/restart');

// --- Authentication --------------------------------------------------------

import type {
  Me,
  Token,
  CreatedToken,
  User,
  UserRole,
  UserPreferencesEnvelope,
  GithubConfig,
  SlackStatus,
} from './types';

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
export const addUser = (
  username: string,
  githubLogin: string | undefined,
  password: string | undefined,
  role: UserRole,
) =>
  post('/auth/users', {
    username,
    github_login: githubLogin || null,
    password: password || null,
    role,
  }) as Promise<User>;

/** Change an approved user's role. */
export const setUserRole = (username: string, role: UserRole) =>
  put(`/auth/users/${encodeURIComponent(username)}/role`, { role }) as Promise<User>;

/** Remove an approved operator. */
export const removeUser = (username: string) => del(`/auth/users/${encodeURIComponent(username)}`);

/** Effective preferences for the signed-in user. */
export const getPreferences = () => get('/preferences') as Promise<UserPreferencesEnvelope>;

/** Set personal overrides; null clears a key back to its deployment value. */
export const patchPreferences = (changes: Record<string, string | null>) =>
  patch('/preferences', changes) as Promise<UserPreferencesEnvelope>;

/** The GitHub App / sign-in config (secret withheld). */
export const getGithubConfig = () => get('/auth/github/config') as Promise<GithubConfig>;

/** Set the sign-in OAuth client id, and optionally the secret (omit to leave it). */
export const setGithubConfig = (clientId: string, clientSecret?: string) =>
  put('/auth/github/config', {
    client_id: clientId,
    ...(clientSecret !== undefined ? { client_secret: clientSecret } : {}),
  }) as Promise<GithubConfig>;

// --- Watches ---------------------------------------------------------------

export const listWatches = () => get('/watches') as Promise<Watch[]>;
export const getWatch = (id: string) => get(`/watches/${encodeURIComponent(id)}`) as Promise<Watch>;
export const createWatch = (body: WatchCreateInput) => post('/watches', body) as Promise<Watch>;
export const updateWatch = (id: string, body: WatchUpdateInput) =>
  patch(`/watches/${encodeURIComponent(id)}`, body) as Promise<Watch>;
export const deleteWatch = (id: string) => del(`/watches/${encodeURIComponent(id)}`);
export const listWatchPrograms = () => get('/watches/programs') as Promise<ProgramView[]>;
export const listWatchRuns = (id: string, limit = 50) =>
  get(`/watches/${encodeURIComponent(id)}/runs?limit=${limit}`) as Promise<WatchRun[]>;
export const runWatch = (id: string, dryRun = false) =>
  post(`/watches/${encodeURIComponent(id)}/run`, {
    dry_run: dryRun,
  }) as Promise<WatchRunResult>;

// --- Slack -------------------------------------------------------------

/** Read-only Slack connection state — configured/connected plus the bot
 *  identity or error (`GET /api/slack/status`). */
export const getSlackStatus = () => get('/slack/status') as Promise<SlackStatus>;

// --- Server logs / debug ---------------------------------------------------

/** A snapshot of the most recent server log lines (oldest first). The live tail
 *  is an EventSource on `/api/logs/stream`, opened directly by the Logs panel. */
export const getLogs = (limit = 500) =>
  get(`/logs?limit=${limit}`) as Promise<import('./types').LogLine[]>;

/** Build version, pid, and start time of the running server. */
export const getServerStatus = () => get('/status') as Promise<import('./types').ServerStatus>;

/** Redacted durable-state and capacity snapshot for approved operators. */
export const getDiagnostics = () => get('/diagnostics') as Promise<import('./types').Diagnostics>;

/** Recent detached background tasks (the `@loom` webhook launches that run off the
 *  request), newest first. Operator-only, like the log endpoints. */
export const getTasks = () => get('/tasks') as Promise<import('./types').TaskRecord[]>;
