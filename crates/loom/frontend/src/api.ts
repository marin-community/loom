async function request(path: string, opts: RequestInit = {}): Promise<unknown> {
  const res = await fetch('/api' + path, {
    headers: { 'content-type': 'application/json' },
    ...opts,
  });
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

// Send a raw (not JSON-encoded) body — for file uploads and the editor's
// file-write save. The server reads the bytes straight off the request body.
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

import type { Issue } from './types';

/** Every issue across every repo — the Issues pane's cross-repo board. Pass
 *  `all` to include closed issues. */
export const listIssues = (all = false) =>
  get(`/issues${all ? '?all=true' : ''}`) as Promise<Issue[]>;

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

// --- Plans -----------------------------------------------------------------

/** A session's plan (parsed, with task status joined from the ledger). Pass a
 *  slug to select a specific plan when the repo has several. */
export const getPlan = (id: string, slug?: string) =>
  get(`/sessions/${id}/plan${slug ? `?slug=${encodeURIComponent(slug)}` : ''}`);

/** Reconcile a plan against the issue ledger. `apply` writes the delta; omit it
 *  to preview. */
export const syncPlan = (id: string, slug: string, apply: boolean) =>
  post(`/sessions/${id}/plan/sync`, { slug, apply });

/** Write a worktree file (the editor save primitive). Path is worktree-relative. */
export const writeFile = (id: string, path: string, content: string) =>
  rawBody('PUT', `/sessions/${id}/file?path=${encodeURIComponent(path)}`, content);
