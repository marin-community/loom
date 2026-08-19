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

/** Invoke a routine code-registered operation. Its dotted identity maps to a
 * resource-grouped API path (`issues.tags.set` → `/api/issues/tags/set`).
 * Exported so a component with no api.ts wrapper of its own for an operation
 * (a handful of direct callers still exist — see each call site) can reach it
 * without inlining a second copy of this path derivation. */
export const invokeOperation = (operation: string, input: unknown) =>
  post(`/${operation.split('.').map(encodeURIComponent).join('/')}`, input);

// UNMAPPED: `sessions.scratch.limits` is the only registered scratch
// operation. This upload sends the file's bytes as the raw request body, and
// the operation dispatcher only ever decodes a JSON `Input` — no operation
// serves a raw body — so it stays on the legacy route, as do the list and
// delete calls in `ScratchPanel.vue`.
/** Upload one prompt/reference attachment into the session-owned scratch dir. */
export const uploadSessionScratch = (id: string, file: File) =>
  upload(`/sessions/${id}/scratch?name=${encodeURIComponent(file.name)}`, file) as Promise<
    ScratchFile & { path: string }
  >;

/** Server-owned attachment limits shared by launch staging and live Scratch. */
export const getScratchLimits = () =>
  invokeOperation('sessions.scratch.limits', {}) as Promise<ScratchLimits>;

// --- Sessions ----------------------------------------------------------------

// UNMAPPED: `sessions.list` is a different read — it answers `SessionView[]`
// (full session context) and has no `automation` filter, where this is the
// compact `SessionSummary[]` projection the fleet list renders. It also needs an
// `AbortSignal`, which `invokeOperation` does not thread through, so it calls
// `request` directly.
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
  invokeOperation('sessions.get', { session: id }) as Promise<Session>;

// UNMAPPED: no operation reads a session's per-repository GitHub access
// overrides. `permissions.github.grant`/`.revoke` are the write half (used
// below), and the nearest read — `permissions.effective.get` — answers with an
// `EffectivePermissionsView` (allowed operations plus external scope), not the
// `SessionGithubAccess[]` row list this pane renders. A different read, so the
// legacy route stays hand-mounted.
export const getSessionGithubAccess = (id: string) =>
  get(`/sessions/${encodeURIComponent(id)}/github/access`) as Promise<SessionGithubAccess[]>;
/** The `PUT` half of the old `github/access` route is now the two operations
 *  `permissions.github.grant` (mode `write`) and `.revoke` (mode `none`). */
export const setSessionGithubAccess = (
  id: string,
  repository: string,
  mode: SessionGithubAccess['mode'],
) =>
  invokeOperation(mode === 'write' ? 'permissions.github.grant' : 'permissions.github.revoke', {
    repository,
    session: id,
  }) as Promise<SessionGithubAccess>;
export const listPermissionRequests = (id: string, state?: PermissionRequest['state']) => {
  return invokeOperation('permissions.requests.list', {
    session: id,
    state: state ?? null,
  }) as Promise<PermissionRequest[]>;
};
export const createPermissionRequest = (id: string, repository: string, reason: string) =>
  invokeOperation('permissions.requests.create', {
    session: id,
    repository,
    reason,
    mode: 'write',
  }) as Promise<PermissionRequest>;
/** `/permission-requests/{id}/decision` is now the two operations
 *  `permissions.requests.approve` and `permissions.requests.deny`. */
export const decidePermissionRequest = (
  requestId: string,
  decision: 'approve' | 'deny',
  reason = '',
) =>
  invokeOperation(
    decision === 'approve' ? 'permissions.requests.approve' : 'permissions.requests.deny',
    { request: requestId, reason },
  ) as Promise<PermissionRequest>;

/** Durable automation launch reservations, including failures that never
 *  produced a usable session (`GET /api/runs`). */
export const listRuns = () => invokeOperation('runs.list', {}) as Promise<AutomationRun[]>;
export const archiveSession = (id: string) => invokeOperation('sessions.archive', { session: id });
/** Delete a session outright (the irreversible counterpart of `archiveSession`).
 *  `sessions.delete` answers `200` with a `{ deleted, kind, warnings }` result
 *  where the legacy `DELETE` answered `204` with an empty body; no caller reads
 *  the value, so it stays unannotated here rather than adding a response type. */
export const removeSession = (id: string) => invokeOperation('sessions.delete', { session: id });
export const clearSessionTag = (id: string, key: string) =>
  invokeOperation('sessions.tags.delete', { key, session: id });
export const regenerateSessionTitle = (id: string) =>
  invokeOperation('sessions.title.regenerate', { session: id }) as Promise<Session>;
export const setSessionTitleGeneration = (id: string, enabled: boolean) =>
  invokeOperation('sessions.title.generation.set', { enabled, session: id }) as Promise<Session>;
/** The stored cue, if one has been generated — a pure read. */
export const getResumptionCue = (id: string) =>
  invokeOperation('sessions.resumption_cue.get', { session: id }) as Promise<
    import('./types').ResumptionCue
  >;
/** Generate the cue when it is missing or stale (`force` regenerates it
 *  unconditionally) — the separate write half of the pair above. */
export const ensureResumptionCue = (id: string, force = false) =>
  invokeOperation('sessions.resumption_cue.ensure', { force, session: id }) as Promise<
    import('./types').ResumptionCue
  >;

// --- Session layout ---------------------------------------------------------

export const getSessionLayout = () =>
  invokeOperation('session_layout.get', {}) as Promise<SessionLayout>;
export const createSessionSpace = (name: string, expectedRevision: number) =>
  invokeOperation('session_layout.spaces.create', {
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const updateSessionSpace = (id: string, name: string, expectedRevision: number) =>
  invokeOperation('session_layout.spaces.update', {
    id,
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const deleteSessionSpace = (
  id: string,
  destinationGroupId: string | null,
  expectedRevision: number,
) =>
  invokeOperation('session_layout.spaces.delete', {
    id,
    destination_group_id: destinationGroupId,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const createSessionGroup = (spaceId: string, name: string, expectedRevision: number) =>
  invokeOperation('session_layout.groups.create', {
    space_id: spaceId,
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const updateSessionGroup = (id: string, name: string, expectedRevision: number) =>
  invokeOperation('session_layout.groups.update', {
    id,
    name,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const deleteSessionGroup = (
  id: string,
  destinationGroupId: string | null,
  expectedRevision: number,
) =>
  invokeOperation('session_layout.groups.delete', {
    id,
    destination_group_id: destinationGroupId,
    expected_revision: expectedRevision,
  }) as Promise<SessionLayout>;
export const reorderSessionLayout = (body: {
  kind: SessionLayoutItemKind;
  id: string;
  before_id?: string | null;
  destination_space_id?: string | null;
  expected_revision: number;
}) => invokeOperation('session_layout.reorder', body) as Promise<SessionLayout>;
export const moveSessions = (body: {
  session_ids: string[];
  destination_group_id: string;
  before_session_id?: string | null;
  expected_revision: number;
}) => invokeOperation('session_layout.move', body) as Promise<SessionLayout>;
export const restoreSessionGroups = (body: {
  groups: SessionGroupOrder[];
  expected_revision: number;
}) => invokeOperation('session_layout.restore', body) as Promise<SessionLayout>;
export const setSessionGroupPreference = (groupId: string, collapsed: boolean) =>
  invokeOperation('session_layout.groups.preference.set', {
    id: groupId,
    collapsed,
  }) as Promise<SessionLayout>;

// --- Issues ----------------------------------------------------------------

import type {
  Issue,
  IssueAction,
  IssueActionsResult,
  IssueTagInput,
  Session,
  PermissionRequest,
  SessionGithubAccess,
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

// `branch` is `[context]` on every channels.* operation, but the dispatcher
// can only auto-fill it for a session credential; the dashboard is a human
// (`User`) caller with no branch of its own, and every handler here ignores
// `branch` for anything but the (no-op, for a human) scope check, so it is
// safely omitted throughout this section.
export const listChannels = (archived = false) =>
  invokeOperation('channels.list', { archived }) as Promise<Channel[]>;

export const getChannel = (id: string) =>
  invokeOperation('channels.get', { channel: id }) as Promise<Channel>;

// GAP: `channels.create` requires a `branch` (session) — it calls
// `require_branch` and 404s without one — but this call site offers only a
// bare repo root from a picker with no session in scope. The old repo-scoped
// custom channel has no equivalent under the branch-scoped operation model,
// so this stays on the legacy route.
export const createChannel = (name: string, topic: string, repoRoot: string) =>
  post('/channels', { name, topic, repo_root: repoRoot }) as Promise<Channel>;

// `peek: true` preserves the old route's behavior: listing never advanced the
// read marker on its own (`markChannelRead` below is the explicit, separate
// call sites already use for that) — `channels.messages.list` would otherwise
// mark-read automatically.
export const listChannelMessages = (id: string, after = 0) =>
  invokeOperation('channels.messages.list', {
    channel: id,
    after: Math.max(0, after),
    peek: true,
  }) as Promise<ChannelMessage[]>;

export const sendChannelMessage = (
  id: string,
  body: string,
  kind: ChannelMessage['kind'] = 'message',
  urgency: ChannelMessage['urgency'] = 'normal',
) =>
  invokeOperation('channels.messages.create', {
    channel: id,
    body,
    kind,
    urgency,
    payload: {},
  }) as Promise<ChannelMessage>;

export const markChannelRead = (id: string, seq?: number) =>
  invokeOperation('channels.read_marker.set', {
    channel: id,
    seq,
  }) as Promise<ChannelSubscription>;

// --- Managed repos ---------------------------------------------------------

/** Every registered managed repo — the clone allowlist (`repos.list`). */
export const listRepos = () => invokeOperation('repos.list', {}) as Promise<ManagedRepo[]>;

/** Register a repo (a GitHub `owner/name` slug or clone URL) in the managed
 *  store / allowlist (`repos.register`). Returns the stored mapping. */
export const registerRepo = (repo: string) =>
  invokeOperation('repos.register', { repo }) as Promise<ManagedRepo>;

/** Check that a proposed worktree base resolves to a commit in a local repo. */
export const validateRepoRevision = (cwd: string, revision: string) =>
  invokeOperation('repos.revisions.validate', { cwd, revision }) as Promise<RepoRevisionValidation>;

// --- Your GitHub token (per-user) ------------------------------------------

/** Write-only status for the signed-in user's Loom-stored GitHub PAT. */
export interface GithubTokenStatus {
  set: boolean;
  updated_at: string | null;
}

export const getMyGithubToken = () =>
  invokeOperation('auth.github_token.get', {}) as Promise<GithubTokenStatus>;

/** Store the PAT Loom will inject into this user's ordinary interactive sessions. */
export const setMyGithubToken = (token: string) =>
  invokeOperation('auth.github_token.set', { token }) as Promise<GithubTokenStatus>;

/** Remove the PAT; new sessions fall back to profile-approved GitHub App access. */
export const deleteMyGithubToken = () => invokeOperation('auth.github_token.remove', {});

interface RepoEnvEnvelope {
  repo_root: string;
  env: RepoEnvVar[];
}

/** The per-repo env vars' metadata for a repo (`GET /api/repos/env`). Names and
 *  timestamps only — values are write-only and never returned. */
export const listRepoEnv = (repoRoot: string) =>
  invokeOperation('repos.env.get', { repo_root: repoRoot }).then((r) => (r as RepoEnvEnvelope).env);

/** Upsert one per-repo variable (`repos.env.set`); returns the refreshed
 *  metadata list (no values). */
export const setRepoEnv = (repoRoot: string, name: string, value: string) =>
  invokeOperation('repos.env.set', { repo_root: repoRoot, name, value }).then(
    (r) => (r as RepoEnvEnvelope).env,
  );

/** Delete one per-repo variable (`repos.env.delete`); returns the refreshed
 *  metadata list. */
export const deleteRepoEnv = (repoRoot: string, name: string) =>
  invokeOperation('repos.env.delete', { repo_root: repoRoot, name }).then(
    (r) => (r as RepoEnvEnvelope).env,
  );

interface AgentsEnvelope {
  agents: AgentMetadata[];
  custom: CustomAgent[];
  default_agent: string;
}

export const listAgents = () => invokeOperation('agents.list', {}) as Promise<AgentsEnvelope>;

interface CustomAgentsEnvelope {
  custom: CustomAgent[];
}

/** Define a new custom agent (`agents.custom.create`). Returns the refreshed
 *  custom-agent list. */
export const createCustomAgent = (body: CustomAgentInput) =>
  (invokeOperation('agents.custom.create', body) as Promise<CustomAgentsEnvelope>).then(
    (r) => r.custom,
  );

/** Replace an existing custom agent's definition (`agents.custom.update`; the
 *  name is immutable). Returns the refreshed list. */
export const updateCustomAgent = (name: string, body: CustomAgentInput) =>
  (
    invokeOperation('agents.custom.update', { ...body, name }) as Promise<CustomAgentsEnvelope>
  ).then((r) => r.custom);

/** Delete a custom agent (`agents.custom.delete`). Returns the refreshed list. */
export const deleteCustomAgent = (name: string) =>
  (invokeOperation('agents.custom.delete', { name }) as Promise<CustomAgentsEnvelope>).then(
    (r) => r.custom,
  );

// UNMAPPED: `issues.list` is repository-scoped (`repo_root` required) and has
// no `automation` filter — it is not the same read as this cross-repo,
// automation-aware board. No operation replaces `GET /api/issues`.
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

// GAP: `sessions.launch` exists, but its `title` field is required (no
// default) and this call site never had one to give — the legacy
// `POST /api/sessions` route derived the title server-side from the claimed
// issue. Left on the legacy route rather than inventing a title.
/** Launch a new Loom session that picks up (claims) an existing Loom issue:
 *  the issue's repo is the new session's cwd, and the backend seeds the branch's
 *  title/goal from the issue and stamps it as the tracking (claimed) issue.
 *  Returns the created session view, whose `id` deep-links to its detail page. */
export const launchSessionForIssue = (repoRoot: string, issueId: number) =>
  post('/sessions', { cwd: repoRoot, claim_issue: issueId }) as Promise<Session>;

// GAP: `issues.backlog.create`'s Input has no `tags` field, so the initial
// tag set a caller may have staged in the create-issue form cannot be applied
// atomically with creation (it was already inert before this migration, since
// the operation's Input silently ignores unknown fields).
/** Create an unclaimed repo-level backlog issue and its initial tags atomically. */
export const createRepoIssue = (
  repoRoot: string,
  title: string,
  body = '',
  tags: IssueTagInput[] = [],
) =>
  invokeOperation('issues.backlog.create', {
    repo_root: repoRoot,
    title,
    body,
  }) as Promise<Issue>;

/** Patch an issue's editable fields. Blank `github` unlinks it;
 *  `claimed_branch: null` returns it to the unclaimed backlog.
 *
 *  UNMAPPED: no `issues.update` operation exists for a single-issue partial
 *  edit. `issues.close`/`issues.reopen` cover the `status` variant but take
 *  an id array and return a bulk `IssueActionsResult`, not a single `Issue`
 *  — a different shape, so not a substitute here. */
export const patchIssue = (
  id: number,
  body: Partial<Pick<Issue, 'title' | 'body' | 'status'>> & {
    github?: string;
    claimed_branch?: null;
  },
) => patch(`/issues/${id}`, body) as Promise<Issue>;

/** Refresh, pin, or clear a session's PR association. */
export const refreshSessionGithub = (id: string) =>
  invokeOperation('sessions.github.refresh', { session: id }) as Promise<Session>;
export const setSessionGithub = (id: string, prNumber: number) =>
  invokeOperation('sessions.github.set', { pr_number: prNumber, session: id }) as Promise<Session>;
export const clearSessionGithub = (id: string) =>
  invokeOperation('sessions.github.clear', { session: id }) as Promise<Session>;

/** Delete an issue outright. */
export const deleteIssue = (id: number) =>
  invokeOperation('issues.delete', { ids: [id] }) as Promise<IssueActionsResult>;

/** Apply one action atomically to every issue id. */
export const issueActions = (ids: number[], action: IssueAction) =>
  invokeOperation('issues.actions', { ids, action }) as Promise<IssueActionsResult>;

/** Set (upsert) a free-form label on an issue. */
export const setIssueTag = (id: number, key: string, value: string, note = '') =>
  invokeOperation('issues.tags.set', { id, key, value, note }) as Promise<Issue>;

/** Clear a label on an issue. */
export const clearIssueTag = (id: number, key: string) =>
  invokeOperation('issues.tags.delete', { id, key }) as Promise<Issue>;

// --- Artifacts -------------------------------------------------------------

/** A session's artifacts: its branch-scoped documents plus the repo-shared ones
 *  (a branch-scoped name shadows a shared one). */
export const getArtifacts = (id: string) =>
  invokeOperation('artifacts.list', { branch: id }) as Promise<ArtifactMeta[]>;

/** One artifact — content plus the projected ref map. `rev` selects a revision;
 *  omit it for the latest. */
export const getArtifact = (id: string, name: string, rev?: number) =>
  invokeOperation('artifacts.get', { name, rev, branch: id }) as Promise<ArtifactView>;

/** Write a new revision of an artifact (a user edit, `author: user`), returning
 *  the refreshed view at the new latest revision. */
export const putArtifact = (id: string, name: string, body: ArtifactWriteBody) =>
  invokeOperation('artifacts.write', { name, ...body, branch: id }) as Promise<ArtifactView>;

/** Delete an artifact and its whole revision history — the row the session sees
 *  for that name (its branch-scoped one, else the repo-shared). */
export const deleteArtifact = (id: string, name: string) =>
  invokeOperation('artifacts.delete', { name, branch: id });

/** Availability of the session's embedded editor (code-server). This is
 *  host-level configuration, not session state — `sessions.ide_info` takes no
 *  session id at all, so `id` is unused below. */
export const ideInfo = (_id: string) =>
  invokeOperation('sessions.ide_info', {}) as Promise<IdeInfo>;

// --- Discussion (margin comments) -------------------------------------------

/** Every thread on an artifact — open, resolved, and orphaned alike. */
export const listThreads = (id: string, name: string) =>
  invokeOperation('artifacts.threads.list', { name, branch: id }) as Promise<Thread[]>;

/** Open a new thread anchored to a quoted span, seeded with its first comment. */
export const createThread = (id: string, name: string, body: NewThreadBody) =>
  invokeOperation('artifacts.threads.comment', {
    name,
    body: body.body,
    target: { kind: 'new', base_rev: body.base_rev, anchor: body.anchor },
    branch: id,
  }) as Promise<Thread>;

/** Append a reply to an existing thread.
 *
 *  Note: `artifacts.threads.comment` covers both starting a thread and
 *  replying to one through its `target` union, and its declared `Output` is
 *  the full `Thread` for both — the old branch-scoped reply route used to
 *  return just the new `Comment`. This function is not currently called from
 *  any component, so the widened return type has no caller to update. */
export const addComment = (id: string, name: string, tid: number, body: NewCommentBody) =>
  invokeOperation('artifacts.threads.comment', {
    name,
    body: body.body,
    target: { kind: 'reply', thread_id: tid },
    branch: id,
  }) as Promise<Thread>;

/** Mark a thread resolved. */
export const resolveThread = (id: string, name: string, tid: number) =>
  invokeOperation('artifacts.threads.resolve', {
    name,
    thread_id: tid,
    branch: id,
  }) as Promise<Thread>;

// --- Staged reviews --------------------------------------------------------

// `reviews.list` is one operation for both reads below, discriminated by the
// `subject_kind`/`subject_key` pair exactly as the legacy query string was.
export const listArtifactReviews = (id: string, name: string) =>
  invokeOperation('reviews.list', {
    subject_kind: 'artifact',
    subject_key: name,
    session: id,
  }) as Promise<Review[]>;

export const getChanges = (id: string) =>
  invokeOperation('sessions.changes', { session: id }) as Promise<ChangeSet>;

export const listChangesReviews = (id: string) =>
  invokeOperation('reviews.list', {
    subject_kind: 'changes',
    subject_key: 'changes',
    session: id,
  }) as Promise<Review[]>;

// `reviews.create` takes the reviewed session as an ordinary operand (not a
// context field): the caller here is always a human operator, who has no
// session of their own for the dispatcher to fall back to.
export const createReview = (id: string, body: CreateReviewBody) =>
  invokeOperation('reviews.create', { session: id, ...body }) as Promise<Review>;

export const addReviewComment = (reviewId: number, body: AddReviewCommentBody) =>
  invokeOperation('reviews.comments.create', { id: reviewId, ...body }) as Promise<Review>;

export const updateReviewComment = (
  reviewId: number,
  commentId: number,
  body: UpdateReviewCommentBody,
) =>
  invokeOperation('reviews.comments.update', {
    id: reviewId,
    comment_id: commentId,
    ...body,
  }) as Promise<Review>;

export const updateReview = (reviewId: number, body: UpdateReviewBody) =>
  invokeOperation('reviews.update', { id: reviewId, ...body }) as Promise<Review>;

export const deleteReviewComment = (
  reviewId: number,
  commentId: number,
  expectedRevision: number,
) =>
  invokeOperation('reviews.comments.delete', {
    id: reviewId,
    comment_id: commentId,
    expected_revision: expectedRevision,
  }) as Promise<Review>;

/** `reviews.discard` answers `200` with `{ discarded: true }` where the legacy
 *  `DELETE` answered `204` with an empty body. No caller reads the value. */
export const discardReview = (reviewId: number, expectedRevision: number) =>
  invokeOperation('reviews.discard', { id: reviewId, expected_revision: expectedRevision });

export const submitReview = (
  reviewId: number,
  body: { expected_revision: number; acknowledge_outdated: boolean },
) => invokeOperation('reviews.submit', { id: reviewId, ...body }) as Promise<Review>;

export const retargetReviewToCurrent = (reviewId: number, expectedRevision: number) =>
  invokeOperation('reviews.retarget', {
    id: reviewId,
    expected_revision: expectedRevision,
  }) as Promise<Review>;

export const retryReviewDelivery = (reviewId: number) =>
  invokeOperation('reviews.retry_delivery', { id: reviewId }) as Promise<Review>;

export const setReviewCommentResolution = (
  reviewId: number,
  commentId: number,
  resolved: boolean,
) =>
  invokeOperation('reviews.comments.resolve', {
    id: reviewId,
    comment_id: commentId,
    resolved,
  }) as Promise<ReviewComment>;

/** Type a message into the session's agent pane and, by default, submit it with
 *  Enter to trigger a round (the same primitive the `loom` CLI's `send` wraps).
 *  Requires a live terminal — a torn-down or orphaned session 409s. */
export const sendMessage = (id: string, text: string, submit = true) =>
  invokeOperation('sessions.send', { text, submit, session: id });

/** Replace the provider behind an idle ACP session while preserving its stable
 * loom session, worktree, branch, and canonical conversation journal. */
export const handoffSession = (id: string, body: import('./types').HandoffInput) =>
  invokeOperation('sessions.handoff', { ...body, session: id }) as Promise<Session>;
/** Recover a session: restart a failed live ACP runtime while preserving its
 * worktree/journal, or rebuild and resume an archived session. */
export const recoverSession = (id: string) =>
  invokeOperation('sessions.recover', { session: id }) as Promise<Session>;
/** Resolve a profile-first handoff against an existing session's class and
 * capacity slot before sending its optimistic revisions. */
export const resolveSessionHandoff = (id: string, selection: LaunchSelection) =>
  invokeOperation('sessions.handoff.resolve', {
    selection,
    session: id,
  }) as Promise<ResolvedLaunch>;

// --- ACP conversation (protocol='acp' sessions) ----------------------------

import type { AcpMetadata, ChatSnapshot, PromptAck } from './types';

/** A newest-first page of the journaled ACP conversation. The response is in
 * display order and carries an exclusive cursor for the next older page. */
export const getSessionChat = (id: string, before?: { turn: number; seq: number } | null) =>
  invokeOperation('sessions.chat', {
    before_turn: before?.turn,
    before_seq: before?.seq,
    session: id,
  }) as Promise<ChatSnapshot>;

// `sessions.prompt.create` serves both ACP prompt paths. It takes no `by`: the
// legacy body's caller-supplied author is gone, and provenance is derived from
// the credential instead (a dashboard call is a human, so it records `manual`).
/** Send a user message to an ACP session now, stopping and replacing a live turn. */
export const promptSession = (id: string, text: string, files: string[] = []) =>
  invokeOperation('sessions.prompt.create', {
    text,
    send_now: true,
    files,
    session: id,
  }) as Promise<PromptAck>;

/** Send all durable next-turn feedback now, stopping a live turn first. */
export const forceQueuedSession = (id: string) =>
  invokeOperation('sessions.prompt.create', {
    text: '',
    force_queued: true,
    files: [],
    session: id,
  }) as Promise<PromptAck>;

/** Atomically pull unseen next-turn feedback out of the server queue so it can
 * be edited in the composer. A 409 means the current ACP state has no queue
 * available to retract. */
export const retractQueuedSession = (id: string) =>
  invokeOperation('sessions.prompt.retract', { session: id }) as Promise<{ text: string }>;

/** Worktree-backed completion for `@file` mentions in the ACP composer. */
export const listSessionFiles = (id: string, query: string) =>
  invokeOperation('sessions.files', { q: query, session: id }) as Promise<{ files: string[] }>;

/** Interrupt the in-flight turn: `session/cancel` for an ACP session, an Escape
 *  keystroke for a terminal one. */
export const interruptSession = (id: string) =>
  invokeOperation('sessions.interrupt', { session: id }) as Promise<{ interrupted: boolean }>;

/** Answer a pending permission request (`{option_id}`). 404 for an unknown id,
 *  409 when it was already resolved. `sessions.permissions.answer` — distinct
 *  from `permissions.requests.approve`/`.deny`, which decide a human-approved
 *  GitHub-access request rather than an agent runtime's tool-call prompt — does
 *  still accept `by` (a watch name, blank for `manual`), unlike
 *  `sessions.prompt.create`, so the parameter stays. */
export const answerPermission = (id: string, requestId: string, optionId: string, by?: string) =>
  invokeOperation('sessions.permissions.answer', {
    request_id: requestId,
    option_id: optionId,
    by,
    session: id,
  }) as Promise<{ resolved: boolean; option_id: string }>;

/** Change an ACP session's mode (`session/set_mode`). */
export const setSessionMode = (id: string, modeId: string, by?: string) =>
  invokeOperation('sessions.mode', { mode_id: modeId, by, session: id }) as Promise<{
    mode_id: string;
  }>;

/** Change an agent-owned ACP session configuration selector (model, reasoning
 * effort, or an adapter-specific option). */
export const setSessionConfigOption = (id: string, configId: string, value: string | boolean) =>
  invokeOperation('sessions.config.set', {
    config_id: configId,
    value,
    session: id,
  }) as Promise<{
    config_id: string;
    value: string | boolean;
    metadata: AcpMetadata;
  }>;

// --- Agent environment variables -------------------------------------------

import type { EnvVar } from './types';

/** The operator-managed env vars exported into every agent session.
 *  `settings.env.list`/`.set`/`.delete` return the list directly now — no
 *  envelope to unwrap. */
export const listEnv = () => invokeOperation('settings.env.list', {}) as Promise<EnvVar[]>;

/** Upsert a variable by name; returns the refreshed list. */
export const setEnv = (name: string, value: string) =>
  invokeOperation('settings.env.set', { name, value }) as Promise<EnvVar[]>;

/** Delete a variable by name; returns the refreshed list. */
export const deleteEnv = (name: string) =>
  invokeOperation('settings.env.delete', { name }) as Promise<EnvVar[]>;

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

export const listProfiles = () => invokeOperation('profiles.list', {}) as Promise<Profile[]>;
export const resolveSessionLaunch = (selection: LaunchSelection) =>
  invokeOperation('sessions.launches.resolve', { selection }) as Promise<ResolvedLaunch>;
export const getMcpRegistry = () =>
  invokeOperation('mcps.get', {}) as Promise<import('./types').McpRegistry>;
export const createCustomMcp = (input: CustomMcpInput) =>
  invokeOperation('mcps.custom.create', input) as Promise<CustomMcp>;
export const updateCustomMcp = (identity: string, input: CustomMcpInput) =>
  invokeOperation('mcps.custom.update', {
    ...input,
    identity: identity.replace(/^\/+/, ''),
  }) as Promise<CustomMcp>;
export const deleteCustomMcp = (identity: string) =>
  invokeOperation('mcps.custom.delete', { identity: identity.replace(/^\/+/, '') });
export const createProfile = (profile: ProfileInput) =>
  invokeOperation('profiles.create', profile) as Promise<Profile>;
export const updateProfile = (name: string, profile: ProfileInput) =>
  invokeOperation('profiles.update', { ...profile, name }) as Promise<Profile>;
export const cloneProfile = (source: string, input: CloneProfileInput) =>
  invokeOperation('profiles.clone', { source, ...input }) as Promise<Profile>;
export const deleteProfile = (name: string) => invokeOperation('profiles.delete', { name });
export const setProfileEnv = (profile: string, name: string, value: string) =>
  invokeOperation('profiles.env.set', { profile, name, value }) as Promise<Profile>;
export const deleteProfileEnv = (profile: string, name: string) =>
  invokeOperation('profiles.env.delete', { profile, name }) as Promise<Profile>;

/** Reset the operator scratch shell — kill it and spawn a fresh login shell. */
export const restartShell = () => invokeOperation('shell.restart', {});

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

/** Who the caller is + which sign-in methods to offer. Never 401s.
 *  `auth.me` answers an unauthenticated caller too, so the login screen keeps
 *  working — only the method changed, `GET` to `POST`. */
export const getMe = () => invokeOperation('auth.me', {}) as Promise<Me>;

/** Username/password login; sets the session cookie on success. */
export const login = (username: string, password: string) =>
  invokeOperation('auth.login', { username, password });

/** Drop the session and clear the cookie. */
export const logout = () => invokeOperation('auth.logout', {});

/** Begin GitHub OAuth — a full-page navigation (the server 302s to GitHub). */
export const githubLoginUrl = '/api/auth/github/login';

/** The user-managed API tokens. */
export const listTokens = () => invokeOperation('auth.tokens.list', {}) as Promise<Token[]>;

/** Mint a token; the plaintext is in the reply once and never again. */
export const createToken = (name: string, expiresInDays?: number | null) =>
  invokeOperation('auth.tokens.create', {
    name,
    expires_in_days: expiresInDays ?? null,
  }) as Promise<CreatedToken>;

/** Revoke a token by id. */
export const revokeToken = (id: string) => invokeOperation('auth.tokens.revoke', { id });

/** Set/change the caller's own password. */
export const setPassword = (newPassword: string) =>
  invokeOperation('auth.set_password', { new_password: newPassword });

/** The approved-operator allowlist. */
export const listUsers = () => invokeOperation('auth.users.list', {}) as Promise<User[]>;

/** Approve a new operator (GitHub login and/or password). */
export const addUser = (
  username: string,
  githubLogin: string | undefined,
  password: string | undefined,
  role: UserRole,
) =>
  invokeOperation('auth.users.create', {
    username,
    github_login: githubLogin || null,
    password: password || null,
    role,
  }) as Promise<User>;

/** Change an approved user's role. */
export const setUserRole = (username: string, role: UserRole) =>
  invokeOperation('auth.users.set_role', { username, role }) as Promise<User>;

/** Remove an approved operator. */
export const removeUser = (username: string) => invokeOperation('auth.users.remove', { username });

/** Effective preferences for the signed-in user. */
export const getPreferences = () =>
  invokeOperation('preferences.get', {}) as Promise<UserPreferencesEnvelope>;

/** Set personal overrides; null clears a key back to its deployment value. */
export const patchPreferences = (changes: Record<string, string | null>) =>
  invokeOperation('preferences.patch', { changes }) as Promise<UserPreferencesEnvelope>;

/** The GitHub App / sign-in config (secret withheld). */
export const getGithubConfig = () =>
  invokeOperation('auth.github_config.get', {}) as Promise<GithubConfig>;

/** Set the sign-in OAuth client id, and optionally the secret (omit to leave it). */
export const setGithubConfig = (clientId: string, clientSecret?: string) =>
  invokeOperation('auth.github_config.set', {
    client_id: clientId,
    ...(clientSecret !== undefined ? { client_secret: clientSecret } : {}),
  }) as Promise<GithubConfig>;

// --- Watches ---------------------------------------------------------------

export const listWatches = () => invokeOperation('watches.list', {}) as Promise<Watch[]>;
export const getWatch = (id: string) =>
  invokeOperation('watches.get', { key: id }) as Promise<Watch>;
export const createWatch = (body: WatchCreateInput) =>
  invokeOperation('watches.create', body) as Promise<Watch>;
export const updateWatch = (id: string, body: WatchUpdateInput) =>
  invokeOperation('watches.update', { ...body, key: id }) as Promise<Watch>;
export const deleteWatch = (id: string) => invokeOperation('watches.delete', { key: id });
export const listWatchPrograms = () =>
  invokeOperation('watches.programs', {}) as Promise<ProgramView[]>;
export const listWatchRuns = (id: string, limit = 50) =>
  invokeOperation('watches.runs', { key: id, limit }) as Promise<WatchRun[]>;
export const runWatch = (id: string, dryRun = false) =>
  invokeOperation('watches.run', { key: id, dry_run: dryRun }) as Promise<WatchRunResult>;

// --- Slack -------------------------------------------------------------

/** Read-only Slack connection state — configured/connected plus the bot
 *  identity or error (`slack.connection_status`). */
export const getSlackStatus = () =>
  invokeOperation('slack.connection_status', {}) as Promise<SlackStatus>;

// --- Server logs / debug ---------------------------------------------------

/** A snapshot of the most recent server log lines (oldest first). The live tail
 *  is an EventSource on `logs.stream`, opened directly by the Logs panel. */
export const getLogs = (limit = 500) =>
  invokeOperation('logs.list', { limit }) as Promise<import('./types').LogLine[]>;

/** Build version, pid, and start time of the running server. */
export const getServerStatus = () =>
  invokeOperation('diagnostics.status', {}) as Promise<import('./types').ServerStatus>;

/** Redacted durable-state and capacity snapshot for approved operators. */
export const getDiagnostics = () =>
  invokeOperation('diagnostics.get', {}) as Promise<import('./types').Diagnostics>;

/** Recent detached background tasks (the `@loom` webhook launches that run off the
 *  request), newest first. Operator-only, like the log endpoints. */
export const getTasks = () =>
  invokeOperation('tasks.list', {}) as Promise<import('./types').TaskRecord[]>;
