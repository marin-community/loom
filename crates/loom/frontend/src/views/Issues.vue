<script setup lang="ts">
import { ref, reactive, computed, nextTick, onMounted, onActivated, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import {
  ApiError,
  listSessions,
  listIssues,
  createRepoIssue,
  patchIssue,
  deleteIssue,
  issueActions,
  setIssueTag,
  clearIssueTag,
  launchSessionForIssue,
} from '../api';
import type { Issue, IssueAction, IssueActionProblem, Session, Tag } from '../types';
import ConfirmDialog from '../components/ConfirmDialog.vue';
import TagPill from '../components/TagPill.vue';
import { timeAgo } from '../lib/time';

// Named so App.vue's <keep-alive :include> keeps this view warm across nav.
defineOptions({ name: 'Issues' });

const router = useRouter();
const route = useRoute();

// The Issues pane — the cross-repo weaver issue board, sibling to the session
// list and the watch panel. API-first: every row is an `IssueView` from
// `GET /api/issues`, every control a REST call. Issues are repo-scoped data, so
// the whole fleet's issues land here and a repo chip / filter disambiguates when
// more than one repo is in play.
//
// What you can do: create a new backlog issue (the "New issue" form), and per
// issue click the title to edit (title + body), close / reopen, delete, and
// manage its free-form `(key, value)` tags as deletable pills. The sessions that
// reference an issue — the branch working it (`claimed`) and the branch it came
// from (`source`) — resolve to live session links from the session list.

const issues = ref<Issue[]>([]);
const sessions = ref<Session[]>([]);
const loaded = ref(false);
const error = ref('');

// Client-side filters over the full (all-status) fetch — at fleet scale the
// whole board is a cheap single GET, so toggles never re-hit the server.
const showClosed = ref(false);
// Issues claimed by an automation-class session (a background agent/watch/
// trigger, not a person) are hidden by default — same shape as showClosed —
// so the board reads as the human backlog unless asked to widen.
const showAutomation = ref(false);
const search = ref('');
const repoFilter = ref('');

// A session badge can deep-link to `/issues?repo_root=…&branch=…`. This scope
// is stronger than the ordinary filters: it is always visible in the toolbar,
// applies before pagination/selection, and changing it clears any prior
// selection so a batch cannot silently cross the URL boundary.
const scopeRepo = computed(() =>
  typeof route.query.repo_root === 'string' ? route.query.repo_root : '',
);
const scopeBranch = computed(() =>
  typeof route.query.branch === 'string' ? route.query.branch : '',
);
const scopeKey = computed(() => `${scopeRepo.value}\0${scopeBranch.value}`);
const hasScope = computed(() => Boolean(scopeRepo.value || scopeBranch.value));

function clearScope() {
  const query = { ...route.query };
  delete query.repo_root;
  delete query.branch;
  router.replace({ query });
}

// Per-issue UI state: which row's editor is open, the edit draft, the per-row
// new-tag input, and a busy flag that disables a row's controls mid-call.
const editing = ref<number | null>(null);
const draft = reactive<{ title: string; body: string; github: string }>({
  title: '',
  body: '',
  github: '',
});
const newTag = reactive<Record<number, string>>({});
const busy = reactive<Record<number, boolean>>({});

async function load() {
  try {
    // Fetch everything (including closed issues, archived sessions, and
    // automation-claimed issues/sessions) once; `showClosed`/`showAutomation`
    // filter client-side. The API hides archived and automation by default, so
    // ask for both.
    const [iss, ses] = await Promise.all([
      listIssues({ all: true, automation: true }),
      listSessions({ archived: true, automation: true }),
    ]);
    issues.value = iss;
    sessions.value = ses;
    error.value = '';
    return true;
  } catch (e) {
    error.value = (e as Error).message;
    return false;
  } finally {
    loaded.value = true;
  }
}

onMounted(load);
// Kept alive across navigation (App.vue), so refresh the board on every return —
// otherwise it would show whatever it held when last left. Guarded so the initial
// mount (already loaded above) doesn't fetch twice.
let firstActivate = true;
onActivated(() => {
  if (firstActivate) {
    firstActivate = false;
    return;
  }
  load();
});

// The short repo label is the last path segment of the repo root.
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}

// Distinct repos present, for the repo filter and the per-row chip (shown only
// when the board spans more than one repo).
const repos = computed(() => [...new Set(issues.value.map((i) => i.repo_root))].sort());
const multiRepo = computed(() => repos.value.length > 1);

// --- Create issue ----------------------------------------------------------
// The "New issue" form files an unclaimed backlog item via `POST /repos/issues`.
// Initial tags travel in that create command and persist atomically with it.
const showCreate = ref(false);
const createRepo = ref('');
const createDraft = reactive<{ title: string; body: string }>({ title: '', body: '' });
const createTags = ref<Tag[]>([]);
const createTagInput = ref('');
const creating = ref(false);
const createError = ref('');
const createTitleInput = ref<HTMLInputElement | null>(null);

// Repos a new issue can target: those already on the board, union the live
// sessions' repos — the repositories in play across the fleet. Empty only when
// nothing is loaded, in which case the form falls back to a free-text path.
const repoChoices = computed(() => {
  const set = new Set<string>();
  if (scopeRepo.value) set.add(scopeRepo.value);
  for (const i of issues.value) set.add(i.repo_root);
  for (const s of sessions.value) set.add(s.branch.repo_root);
  return [...set].sort();
});

async function openCreate() {
  showCreate.value = true;
  createError.value = '';
  // The URL scope is authoritative, then the ordinary repo filter / first repo.
  createRepo.value = scopeRepo.value || repoFilter.value || repoChoices.value[0] || '';
  await nextTick();
  createTitleInput.value?.focus();
}

function cancelCreate() {
  showCreate.value = false;
  createDraft.title = '';
  createDraft.body = '';
  createTags.value = [];
  createTagInput.value = '';
  createError.value = '';
}

// Stage a tag on the not-yet-created issue, reusing the row editor's parser. A
// repeated key replaces the earlier pending value (an upsert, as on the server).
function addCreateTag() {
  const parsed = parseTag(createTagInput.value);
  if (!parsed) {
    createError.value = 'tag must be "key: value" (a value is required)';
    return;
  }
  createError.value = '';
  const tag: Tag = { key: parsed.key, value: parsed.value, note: '', set_by: 'manual', set_at: '' };
  const at = createTags.value.findIndex((t) => t.key === tag.key);
  if (at >= 0) createTags.value[at] = tag;
  else createTags.value.push(tag);
  createTagInput.value = '';
}

function removeCreateTag(key: string) {
  createTags.value = createTags.value.filter((t) => t.key !== key);
}

async function submitCreate() {
  const title = createDraft.title.trim();
  if (!title) {
    createError.value = 'issue title is required';
    return;
  }
  const repo = createRepo.value.trim();
  if (!repo) {
    createError.value = 'a repository is required';
    return;
  }
  creating.value = true;
  createError.value = '';
  try {
    const tags = createTags.value.map(({ key, value, note }) => ({ key, value, note }));
    const created = await createRepoIssue(repo, title, createDraft.body, tags);
    issues.value.unshift(created);
    // Surface the new issue even if a different-repo filter is active.
    if (repoFilter.value && repoFilter.value !== created.repo_root) repoFilter.value = '';
    cancelCreate();
  } catch (e) {
    createError.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

const scoped = computed(() =>
  issues.value.filter((i) => {
    if (scopeRepo.value && i.repo_root !== scopeRepo.value) return false;
    if (
      scopeBranch.value &&
      i.claimed_branch !== scopeBranch.value &&
      i.source_branch !== scopeBranch.value
    ) {
      return false;
    }
    return true;
  }),
);

const visible = computed(() => {
  const q = search.value.trim().toLowerCase();
  return scoped.value.filter((i) => {
    if (!showClosed.value && i.status !== 'open') return false;
    if (!showAutomation.value && isAutomationClaimed(i)) return false;
    if (repoFilter.value && i.repo_root !== repoFilter.value) return false;
    if (!q) return true;
    const hay = [`#${i.id}`, i.title, i.body, ...i.tags.map((t) => `${t.key} ${t.value}`)]
      .join(' ')
      .toLowerCase();
    return hay.includes(q);
  });
});

const openCount = computed(() => scoped.value.filter((i) => i.status === 'open').length);

// Dense client-side pages keep the board quick to scan while selection remains
// ID-based across pages, filters, kept-alive activation, and API refreshes.
const PAGE_SIZE = 25;
const page = ref(1);
const pageCount = computed(() => Math.max(1, Math.ceil(visible.value.length / PAGE_SIZE)));
const pageIssues = computed(() =>
  visible.value.slice((page.value - 1) * PAGE_SIZE, page.value * PAGE_SIZE),
);
watch(
  () => visible.value.length,
  () => {
    page.value = Math.min(page.value, pageCount.value);
  },
);

const selected = ref<Set<number>>(new Set());
const lastSelected = ref<number | null>(null);
const selectedCount = computed(() => selected.value.size);
const selectedOnPage = computed(
  () => pageIssues.value.filter((issue) => selected.value.has(issue.id)).length,
);
const allPageSelected = computed(
  () => pageIssues.value.length > 0 && selectedOnPage.value === pageIssues.value.length,
);
const selectedMatching = computed(
  () => visible.value.filter((issue) => selected.value.has(issue.id)).length,
);
const allMatchingSelected = computed(
  () => visible.value.length > 0 && selectedMatching.value === visible.value.length,
);

function replaceSelection(next: Set<number>) {
  selected.value = next;
}

function toggleSelection(issue: Issue, event: MouseEvent) {
  const next = new Set(selected.value);
  const selecting = !next.has(issue.id);
  if (event.shiftKey && lastSelected.value != null) {
    const from = visible.value.findIndex((candidate) => candidate.id === lastSelected.value);
    const to = visible.value.findIndex((candidate) => candidate.id === issue.id);
    if (from >= 0 && to >= 0) {
      const [start, end] = from < to ? [from, to] : [to, from];
      for (const candidate of visible.value.slice(start, end + 1)) {
        if (selecting) next.add(candidate.id);
        else next.delete(candidate.id);
      }
    }
  } else if (selecting) {
    next.add(issue.id);
  } else {
    next.delete(issue.id);
  }
  lastSelected.value = issue.id;
  replaceSelection(next);
}

function toggleVisibleSelection() {
  const next = new Set(selected.value);
  for (const issue of pageIssues.value) {
    if (allPageSelected.value) next.delete(issue.id);
    else next.add(issue.id);
  }
  replaceSelection(next);
}

function selectAllMatching() {
  const next = new Set(selected.value);
  for (const issue of visible.value) next.add(issue.id);
  replaceSelection(next);
}

function clearSelection() {
  replaceSelection(new Set());
  lastSelected.value = null;
}

watch(scopeKey, (_next, previous) => {
  if (previous !== undefined) clearSelection();
  page.value = 1;
});

// Sessions indexed by `(repo_root, branch)` so a row resolves its references
// with a map lookup instead of rescanning the whole session list — `refsFor`
// runs several times per row, so the rebuilt-on-change index keeps rendering
// off the O(issues × sessions) path.
const sessionsByBranch = computed(() => {
  const m = new Map<string, Session[]>();
  for (const s of sessions.value) {
    const k = `${s.branch.repo_root}\0${s.branch.branch}`;
    const arr = m.get(k);
    if (arr) arr.push(s);
    else m.set(k, [s]);
  }
  return m;
});

// An issue is automation-claimed when the session *currently* working its
// branch (`claimed_branch`) is automation-class. Archived sessions never own
// work, including historical rows whose claims predate archive cleanup. Mirrors
// the server's own default-hide rule (`GET /api/issues`).
function isAutomationClaimed(i: Issue): boolean {
  if (!i.claimed_branch) return false;
  const held = sessionsByBranch.value.get(`${i.repo_root}\0${i.claimed_branch}`) ?? [];
  const eligible = held.filter((s) => s.status !== 'archived');
  const active = (s: Session) => !['done', 'error'].includes(s.status);
  const holder = eligible.sort(
    (a, b) => Number(active(b)) - Number(active(a)) || b.created_at.localeCompare(a.created_at),
  )[0];
  return holder?.class === 'automation';
}

// Sessions that reference an issue: the branch working it (claimed) and the
// branch it came from (source), matched against the live session list by
// repo + branch name. Claimed first, deduped, each tagged with its relation.
function refsFor(i: Issue): { session: Session; rel: string }[] {
  const out: { session: Session; rel: string }[] = [];
  const seen = new Set<string>();
  const match = (branch: string | null, rel: string) => {
    if (!branch) return;
    for (const s of sessionsByBranch.value.get(`${i.repo_root}\0${branch}`) ?? []) {
      if (!seen.has(s.id)) {
        seen.add(s.id);
        out.push({ session: s, rel });
      }
    }
  };
  match(i.claimed_branch, 'claimed');
  match(i.source_branch, 'from');
  return out;
}

// The branch label to show when no live session matches (the worktree may be
// archived). Strips the `weaver/` prefix the way the rest of the UI does.
function branchLabel(b: string): string {
  return b.replace(/^weaver\//, '');
}

// Replace one issue in place from a mutation's response, so the list updates
// without a full reload. A no-op when the issue isn't in the current view.
function replaceIssue(updated: Issue) {
  const idx = issues.value.findIndex((x) => x.id === updated.id);
  if (idx >= 0) issues.value[idx] = updated;
}

async function withBusy<T>(id: number, fn: () => Promise<T>): Promise<T | undefined> {
  busy[id] = true;
  error.value = '';
  try {
    return await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy[id] = false;
  }
}

async function setStatus(i: Issue, status: 'open' | 'closed') {
  await withBusy(i.id, async () => replaceIssue((await patchIssue(i.id, { status })) as Issue));
}

async function unclaim(i: Issue) {
  await withBusy(i.id, async () =>
    replaceIssue((await patchIssue(i.id, { claimed_branch: null })) as Issue),
  );
}

// Launch a fresh loom session that picks up an unclaimed backlog issue: the
// server forks a branch in the issue's repo, claims the issue as its tracker,
// and seeds the goal from it. On success we follow straight to the new
// session's detail page (so the row's claim-state is re-read on the next visit).
const launching = ref<number | null>(null);
async function launch(i: Issue) {
  launching.value = i.id;
  error.value = '';
  try {
    const session = await launchSessionForIssue(i.repo_root, i.id);
    router.push(`/s/${session.id}`);
  } catch (e) {
    error.value = (e as Error).message;
    launching.value = null;
  }
}

interface BatchFeedback {
  success: boolean;
  message: string;
  problems: IssueActionProblem[];
}

const batchBusy = ref(false);
const batchFeedback = ref<BatchFeedback | null>(null);
const lastBatch = ref<{ ids: number[]; action: IssueAction } | null>(null);

function actionLabel(action: IssueAction): string {
  switch (action.type) {
    case 'close':
      return 'Close';
    case 'reopen':
      return 'Reopen';
    case 'tag':
      return `Tag ${action.key}: ${action.value}`;
    case 'untag':
      return `Remove tag ${action.key}`;
    case 'delete':
      return 'Delete';
  }
}

function actionProblems(error: unknown): IssueActionProblem[] {
  if (!(error instanceof ApiError)) return [];
  const details = error.body.details;
  if (!details || typeof details !== 'object') return [];
  const problems = (details as { problems?: unknown }).problems;
  if (!Array.isArray(problems)) return [];
  return problems.filter(
    (problem): problem is IssueActionProblem =>
      problem != null &&
      typeof problem === 'object' &&
      typeof (problem as IssueActionProblem).id === 'number' &&
      typeof (problem as IssueActionProblem).code === 'string' &&
      typeof (problem as IssueActionProblem).error === 'string',
  );
}

async function runBatch(ids: number[], action: IssueAction): Promise<boolean> {
  if (!ids.length) return false;
  batchBusy.value = true;
  batchFeedback.value = null;
  lastBatch.value = { ids: [...ids], action };
  try {
    const result = await issueActions(ids, action);
    const affected = result.issues.length + result.deleted_ids.length;
    const refreshed = await load();
    clearSelection();
    batchFeedback.value = {
      success: true,
      message: `${actionLabel(action)} applied to ${affected} issue${affected === 1 ? '' : 's'}.${
        refreshed ? '' : ' The list could not be refreshed; retry the refresh below.'
      }`,
      problems: [],
    };
    return true;
  } catch (failure) {
    const problems = actionProblems(failure);
    batchFeedback.value = {
      success: false,
      message: `${(failure as Error).message}. No issues were changed.`,
      problems,
    };
    return false;
  } finally {
    batchBusy.value = false;
  }
}

function runSelected(action: IssueAction) {
  return runBatch([...selected.value], action);
}

function removeProblemIds() {
  const next = new Set(selected.value);
  for (const problem of batchFeedback.value?.problems ?? []) next.delete(problem.id);
  replaceSelection(next);
}

function retryLastBatch() {
  if (!lastBatch.value) return;
  const ids = selected.value.size ? [...selected.value] : lastBatch.value.ids;
  if (lastBatch.value.action.type === 'delete') {
    deleteRequest.value = { ids };
    return;
  }
  runBatch(ids, lastBatch.value.action);
}

const deleteRequest = ref<{ ids: number[]; single?: Issue } | null>(null);

function requestDelete(issue: Issue) {
  deleteRequest.value = { ids: [issue.id], single: issue };
}

function requestBulkDelete() {
  deleteRequest.value = { ids: [...selected.value] };
}

async function confirmDelete() {
  const request = deleteRequest.value;
  if (!request) return;
  if (request.single) {
    const issue = request.single;
    const removed = await withBusy(issue.id, async () => {
      await deleteIssue(issue.id);
      issues.value = issues.value.filter((candidate) => candidate.id !== issue.id);
      if (editing.value === issue.id) editing.value = null;
      const next = new Set(selected.value);
      next.delete(issue.id);
      replaceSelection(next);
      return true;
    });
    if (removed) deleteRequest.value = null;
    return;
  }
  await runBatch(request.ids, { type: 'delete' });
  deleteRequest.value = null;
}

const tagDialogOpen = ref(false);
const tagMode = ref<'tag' | 'untag'>('tag');
const bulkTagKey = ref('');
const bulkTagValue = ref('');
const bulkTagError = ref('');

function openTagDialog() {
  tagMode.value = 'tag';
  bulkTagKey.value = '';
  bulkTagValue.value = '';
  bulkTagError.value = '';
  tagDialogOpen.value = true;
}

async function confirmTag() {
  const key = bulkTagKey.value.trim();
  const value = bulkTagValue.value.trim();
  if (!key) {
    bulkTagError.value = 'A tag key is required.';
    return;
  }
  if (tagMode.value === 'tag' && !value) {
    bulkTagError.value = 'A tag value is required.';
    return;
  }
  bulkTagError.value = '';
  const action: IssueAction =
    tagMode.value === 'tag' ? { type: 'tag', key, value, by: 'manual' } : { type: 'untag', key };
  await runSelected(action);
  tagDialogOpen.value = false;
}

function startEdit(i: Issue) {
  if (editing.value === i.id) {
    editing.value = null;
    return;
  }
  editing.value = i.id;
  draft.title = i.title;
  draft.body = i.body;
  draft.github = i.github_repo && i.github_issue ? `${i.github_repo}#${i.github_issue}` : '';
}

async function saveEdit(i: Issue) {
  const title = draft.title.trim();
  if (!title) {
    error.value = 'issue title is required';
    return;
  }
  await withBusy(i.id, async () => {
    replaceIssue(
      (await patchIssue(i.id, { title, body: draft.body, github: draft.github.trim() })) as Issue,
    );
    editing.value = null;
  });
}

// Parse a `key:value`, `key=value`, or `key value` tag input. A bare key (no
// value) is rejected — issue tags require a non-empty value.
function parseTag(raw: string): { key: string; value: string } | null {
  const trimmed = raw.trim();
  const m = trimmed.match(/^([^\s:=]+)\s*[:=\s]\s*(.+)$/);
  if (!m) return null;
  return { key: m[1].trim(), value: m[2].trim() };
}

async function addTag(i: Issue) {
  const parsed = parseTag(newTag[i.id] ?? '');
  if (!parsed) {
    error.value = 'tag must be "key: value" (a value is required)';
    return;
  }
  await withBusy(i.id, async () => {
    replaceIssue((await setIssueTag(i.id, parsed.key, parsed.value)) as Issue);
    newTag[i.id] = '';
  });
}

async function removeTag(i: Issue, key: string) {
  await withBusy(i.id, async () => replaceIssue((await clearIssueTag(i.id, key)) as Issue));
}
</script>

<template>
  <div class="px-5 py-3">
    <!-- One toolbar line: view label, open count, then the filters pushed
         right — same anatomy as the fleet list's toolbar. -->
    <div class="mb-3 flex min-h-7 flex-wrap items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Issues</h1>
      <span class="pill font-mono" data-testid="issues-open-count">{{ openCount }} open</span>
      <span
        v-if="hasScope"
        class="flex items-center gap-1 rounded bg-info-soft px-2 py-0.5 font-mono text-2xs text-info"
        data-testid="issues-active-scope"
      >
        <span>
          Scoped to
          {{ scopeRepo ? repoName(scopeRepo) : 'all repos'
          }}{{ scopeBranch ? ` / ${branchLabel(scopeBranch)}` : '' }}
        </span>
        <button
          type="button"
          class="rounded px-1 hover:bg-subtle"
          data-testid="issues-clear-scope"
          aria-label="Clear issue scope"
          @click="clearScope"
        >
          ×
        </button>
      </span>

      <div
        v-if="selectedCount"
        class="ml-auto flex flex-wrap items-center gap-1.5"
        data-testid="issues-bulk-toolbar"
      >
        <strong class="mr-1 text-xs text-fg" data-testid="issues-selected-count"
          >{{ selectedCount }} selected</strong
        >
        <span v-if="selectedMatching !== selectedCount" class="mr-1 text-2xs text-muted">
          {{ selectedMatching }} match filters
        </span>
        <input
          v-model="search"
          type="search"
          placeholder="Filter selected view…"
          data-testid="issues-search"
          class="w-40 rounded bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        />
        <button
          v-if="pageIssues.length"
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          data-testid="issues-select-visible"
          @click="toggleVisibleSelection"
        >
          {{ allPageSelected ? 'Deselect' : 'Select' }} visible {{ pageIssues.length }}
        </button>
        <button
          v-if="!allMatchingSelected && visible.length > selectedMatching"
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          data-testid="issues-select-matching"
          @click="selectAllMatching"
        >
          Select all matching {{ visible.length }}
        </button>
        <button
          type="button"
          class="rounded px-2 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
          data-testid="issues-bulk-close"
          :disabled="batchBusy"
          @click="runSelected({ type: 'close' })"
        >
          Close
        </button>
        <button
          type="button"
          class="rounded px-2 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
          data-testid="issues-bulk-reopen"
          :disabled="batchBusy"
          @click="runSelected({ type: 'reopen' })"
        >
          Reopen
        </button>
        <button
          type="button"
          class="rounded px-2 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
          data-testid="issues-bulk-tag"
          :disabled="batchBusy"
          @click="openTagDialog"
        >
          Tag
        </button>
        <button
          type="button"
          class="rounded px-2 py-1 text-xs text-block hover:bg-block-soft"
          data-testid="issues-bulk-delete"
          :disabled="batchBusy"
          @click="requestBulkDelete"
        >
          Delete
        </button>
        <button
          type="button"
          class="rounded px-2 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
          data-testid="issues-selection-clear"
          :disabled="batchBusy"
          @click="clearSelection"
        >
          Clear
        </button>
      </div>

      <div v-else class="ml-auto flex flex-wrap items-center gap-2">
        <button
          v-if="pageIssues.length"
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          data-testid="issues-select-visible"
          @click="toggleVisibleSelection"
        >
          Select visible {{ pageIssues.length }}
        </button>
        <input
          v-model="search"
          type="search"
          placeholder="Filter issues…"
          data-testid="issues-search"
          class="w-48 rounded bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        />
        <select
          v-if="multiRepo && !scopeRepo"
          v-model="repoFilter"
          data-testid="issues-repo-filter"
          class="rounded bg-input px-2 py-1 text-xs text-fg outline-none ring-accent focus:ring-1"
        >
          <option value="">All repos</option>
          <option v-for="r in repos" :key="r" :value="r">{{ repoName(r) }}</option>
        </select>
        <label class="flex items-center gap-1.5 text-xs text-muted">
          <input
            v-model="showClosed"
            type="checkbox"
            class="accent-accent"
            data-testid="issues-show-closed"
          />
          Show closed
        </label>
        <label class="flex items-center gap-1.5 text-xs text-muted">
          <input
            v-model="showAutomation"
            type="checkbox"
            class="accent-accent"
            data-testid="issues-show-automation"
          />
          Show automation
        </label>
        <button
          type="button"
          :class="['px-2.5 py-1 text-xs font-medium', showCreate ? 'btn-secondary' : 'btn-primary']"
          data-testid="issue-create-toggle"
          @click="showCreate ? cancelCreate() : openCreate()"
        >
          {{ showCreate ? 'Cancel' : 'New issue' }}
        </button>
      </div>
    </div>

    <!--
      New-issue form. Grouped into quiet labeled fields (Repository / Title /
      Body / Tags), matching the session create form's light treatment. Files an
      unclaimed backlog item with its staged tags in one atomic create command.
    -->
    <form
      v-if="showCreate"
      class="mb-4 max-w-3xl space-y-4 rounded-md border border-line bg-surface p-4"
      data-testid="issue-create-form"
      @submit.prevent="submitCreate"
    >
      <!-- Repository: the backlog this lands in. A static label when one repo is
           in play, a picker when several, a free path when the board is empty. -->
      <div>
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted"
          >Repository</span
        >
        <select
          v-if="repoChoices.length > 1"
          v-model="createRepo"
          data-testid="issue-create-repo"
          class="w-full rounded bg-input px-2 py-1 text-sm text-fg outline-none ring-accent focus:ring-1"
        >
          <option v-for="r in repoChoices" :key="r" :value="r">{{ repoName(r) }} — {{ r }}</option>
        </select>
        <p
          v-else-if="repoChoices.length === 1"
          class="font-mono text-sm text-muted"
          :title="createRepo"
          data-testid="issue-create-repo"
        >
          {{ repoName(createRepo) }}
        </p>
        <input
          v-else
          v-model="createRepo"
          type="text"
          placeholder="/home/you/code/project"
          data-testid="issue-create-repo"
          class="w-full rounded bg-input px-2 py-1 font-mono text-sm text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        />
      </div>

      <label class="block">
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted"
          >Title</span
        >
        <input
          ref="createTitleInput"
          v-model="createDraft.title"
          type="text"
          placeholder="Short summary of the work"
          data-testid="issue-create-title"
          class="w-full rounded bg-input px-2 py-1 text-sm text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        />
      </label>

      <label class="block">
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted"
          >Body</span
        >
        <textarea
          v-model="createDraft.body"
          rows="4"
          placeholder="Optional detail, acceptance criteria, links…"
          data-testid="issue-create-body"
          class="w-full rounded bg-input px-2 py-1 font-mono text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        ></textarea>
      </label>

      <div>
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted"
          >Tags</span
        >
        <div class="flex flex-wrap items-center gap-1.5">
          <TagPill v-for="t in createTags" :key="t.key" :tag="t" @clear="removeCreateTag" />
          <span class="flex items-center gap-1">
            <input
              v-model="createTagInput"
              type="text"
              placeholder="key: value"
              data-testid="issue-create-tag-input"
              class="w-36 rounded bg-input px-2 py-0.5 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
              @keydown.enter.prevent="addCreateTag"
            />
            <button
              type="button"
              class="btn-secondary px-2 py-0.5 text-xs"
              data-testid="issue-create-tag-add"
              @click="addCreateTag"
            >
              Add
            </button>
          </span>
        </div>
      </div>

      <p v-if="createError" class="text-sm text-block" data-testid="issue-create-error">
        {{ createError }}
      </p>

      <div class="flex items-center gap-2">
        <button
          type="submit"
          class="btn-primary px-2.5 py-1 text-xs font-medium"
          data-testid="issue-create-submit"
          :disabled="creating"
        >
          {{ creating ? 'Creating…' : 'Create issue' }}
        </button>
        <button
          type="button"
          class="btn-secondary px-2.5 py-1 text-xs font-medium"
          :disabled="creating"
          @click="cancelCreate"
        >
          Cancel
        </button>
      </div>
    </form>

    <div
      v-if="batchFeedback"
      :role="batchFeedback.success ? 'status' : 'alert'"
      :class="
        batchFeedback.success
          ? 'border-ok-line bg-ok-soft text-ok'
          : 'border-block-line bg-block-soft text-block'
      "
      class="mb-3 rounded border p-3 text-sm"
      data-testid="issues-batch-feedback"
    >
      <p>{{ batchFeedback.message }}</p>
      <ul v-if="batchFeedback.problems.length" class="mt-2 space-y-1 font-mono text-xs">
        <li v-for="problem in batchFeedback.problems" :key="`${problem.id}-${problem.code}`">
          #{{ problem.id }} — {{ problem.error }}
        </li>
      </ul>
      <div v-if="!batchFeedback.success" class="mt-2 flex gap-2">
        <button
          v-if="batchFeedback.problems.length"
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          data-testid="issues-remove-invalid"
          @click="removeProblemIds"
        >
          Remove invalid from selection
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          data-testid="issues-batch-retry"
          :disabled="batchBusy"
          @click="retryLastBatch"
        >
          Retry batch
        </button>
      </div>
    </div>

    <div
      v-if="error"
      role="alert"
      class="mb-3 flex items-center gap-2 text-sm text-block"
      data-testid="issues-error"
    >
      <span>{{ error }}</span>
      <button type="button" class="btn-secondary px-2 py-0.5 text-xs" @click="load">Retry</button>
    </div>

    <p v-if="!loaded" class="text-sm text-muted">Loading…</p>
    <p
      v-else-if="!visible.length"
      class="rounded-md border border-dashed border-line p-6 text-center text-sm text-faint"
      data-testid="issues-empty"
    >
      {{
        issues.length
          ? hasScope
            ? 'No issues match this scope and the current filters.'
            : 'No issues match the current filters.'
          : 'No issues yet.'
      }}
      <button
        v-if="hasScope"
        type="button"
        class="ml-2 text-accent hover:underline"
        @click="clearScope"
      >
        Clear scope
      </button>
    </p>

    <!-- One bordered board, hairline-divided rows (the fleet-list anatomy).
         Per-row actions are ghost buttons revealed on hover/focus, so the
         board reads as data, not as a wall of buttons. -->
    <ul
      v-else
      class="overflow-hidden rounded-md border border-line bg-surface"
      data-testid="issues-list"
    >
      <li
        v-for="i in pageIssues"
        :key="i.id"
        class="group border-b border-line px-3 py-2 last:border-0 transition-colors hover:bg-subtle/50"
        :class="{
          'opacity-60': i.status !== 'open',
          'bg-info-soft/60': selected.has(i.id),
        }"
        data-testid="issue-row"
        :data-issue-id="i.id"
      >
        <!-- Row 1: status dot · id · title (click to edit) · repo chip ·
             actions (hover-revealed) · freshness -->
        <div class="flex items-center gap-2">
          <input
            type="checkbox"
            class="shrink-0 accent-accent"
            data-testid="issue-select"
            :checked="selected.has(i.id)"
            :aria-label="`Select issue #${i.id}: ${i.title}`"
            @click.stop="toggleSelection(i, $event)"
          />
          <span
            class="flex shrink-0 items-center gap-1.5 font-mono text-2xs"
            :class="i.status === 'open' ? 'text-accent' : 'text-faint'"
            data-testid="issue-status"
          >
            <span
              class="h-1.5 w-1.5 rounded-full"
              :class="i.status === 'open' ? 'bg-accent' : 'bg-faint/60'"
              aria-hidden="true"
            ></span>
            {{ i.status }}
          </span>
          <span class="shrink-0 font-mono text-2xs text-faint">#{{ i.id }}</span>
          <button
            type="button"
            class="min-w-0 flex-1 truncate text-left text-sm text-fg hover:text-accent"
            :class="{ 'line-through decoration-muted': i.status !== 'open' }"
            data-testid="issue-title"
            :title="editing === i.id ? 'Collapse editor' : 'Edit issue'"
            @click="startEdit(i)"
          >
            {{ i.title }}
          </button>
          <span v-if="multiRepo" class="pill shrink-0 font-mono" :title="i.repo_root">{{
            repoName(i.repo_root)
          }}</span>
          <a
            v-if="i.github_issue && i.github_repo"
            :href="`https://github.com/${i.github_repo}/issues/${i.github_issue}`"
            target="_blank"
            rel="noopener"
            class="shrink-0 font-mono text-2xs text-muted hover:text-accent"
            @click.stop
            >gh #{{ i.github_issue }}</a
          >

          <div
            class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
          >
            <!-- Launch a session to work this issue — the lead, accent-tinted
                 action. Offered only for an unclaimed open item; a claimed issue
                 already has its working session (linked in row 2). -->
            <button
              v-if="i.status === 'open' && !i.claimed_branch"
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs font-medium text-accent hover:bg-subtle"
              data-testid="issue-launch"
              :disabled="busy[i.id] || launching === i.id"
              :title="`Launch a session to work issue #${i.id}`"
              @click="launch(i)"
            >
              {{ launching === i.id ? 'Launching…' : 'Launch' }}
            </button>
            <button
              v-if="i.status === 'open'"
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-close"
              :disabled="busy[i.id]"
              @click="setStatus(i, 'closed')"
            >
              Close
            </button>
            <button
              v-else
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-reopen"
              :disabled="busy[i.id]"
              @click="setStatus(i, 'open')"
            >
              Reopen
            </button>
            <button
              v-if="i.claimed_branch"
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-unclaim"
              :disabled="busy[i.id]"
              :title="`Return issue #${i.id} to the unclaimed backlog`"
              @click="unclaim(i)"
            >
              Unclaim
            </button>
            <button
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-edit"
              :disabled="busy[i.id]"
              @click="startEdit(i)"
            >
              {{ editing === i.id ? 'Cancel' : 'Edit' }}
            </button>
            <button
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-block-soft hover:text-block"
              data-testid="issue-delete"
              :disabled="busy[i.id]"
              @click="requestDelete(i)"
            >
              Delete
            </button>
          </div>

          <span
            class="shrink-0 font-mono text-2xs text-faint"
            :title="`updated ${i.updated_at} · created ${i.created_at}`"
            >{{ timeAgo(i.updated_at) }}</span
          >
        </div>

        <!-- Row 2 (only when there's something): tag pills + referencing sessions -->
        <div
          v-if="i.tags.length || refsFor(i).length || i.claimed_branch || i.source_branch"
          class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 pl-[4.5rem] text-xs"
        >
          <div v-if="i.tags.length" class="flex flex-wrap items-center gap-1.5">
            <TagPill
              v-for="t in i.tags"
              :key="t.key"
              :tag="t"
              :busy="busy[i.id]"
              @clear="removeTag(i, $event)"
            />
          </div>

          <div v-if="refsFor(i).length" class="flex flex-wrap items-center gap-1.5 text-muted">
            <span class="text-faint">referenced by</span>
            <template v-for="r in refsFor(i)" :key="r.session.id">
              <router-link
                :to="`/s/${r.session.id}`"
                class="font-mono text-accent hover:underline"
                data-testid="issue-session-ref"
                >{{ r.rel }}: {{ r.session.branch.name }}</router-link
              >
            </template>
          </div>
          <span
            v-else-if="i.claimed_branch || i.source_branch"
            class="font-mono text-faint"
            data-testid="issue-branch-ref"
          >
            {{
              i.claimed_branch
                ? `claimed: ${branchLabel(i.claimed_branch)}`
                : `from: ${branchLabel(i.source_branch!)}`
            }}
          </span>
        </div>

        <!-- Editor (expanded on click): title + body + tag management -->
        <div
          v-if="editing === i.id"
          class="mt-3 space-y-3 rounded border border-line bg-canvas/60 p-3"
          data-testid="issue-editor"
        >
          <label class="block">
            <span class="mb-1 block text-xs text-muted">Title</span>
            <input
              v-model="draft.title"
              type="text"
              data-testid="issue-edit-title"
              class="w-full rounded border border-line bg-input px-2 py-1 text-sm text-fg focus:border-accent focus:outline-none"
            />
          </label>
          <label class="block">
            <span class="mb-1 block text-xs text-muted">Body</span>
            <textarea
              v-model="draft.body"
              rows="4"
              data-testid="issue-edit-body"
              class="w-full rounded border border-line bg-input px-2 py-1 font-mono text-xs text-fg focus:border-accent focus:outline-none"
            ></textarea>
          </label>
          <label class="block">
            <span class="mb-1 block text-xs text-muted">GitHub issue</span>
            <input
              v-model="draft.github"
              type="text"
              placeholder="owner/name#123 — blank to unlink"
              data-testid="issue-edit-github"
              class="w-full rounded border border-line bg-input px-2 py-1 font-mono text-xs text-fg focus:border-accent focus:outline-none"
            />
          </label>

          <div>
            <span class="mb-1 block text-xs text-muted">Tags</span>
            <div class="flex flex-wrap items-center gap-1.5">
              <TagPill
                v-for="t in i.tags"
                :key="t.key"
                :tag="t"
                :busy="busy[i.id]"
                @clear="removeTag(i, $event)"
              />
              <form class="flex items-center gap-1" @submit.prevent="addTag(i)">
                <input
                  v-model="newTag[i.id]"
                  type="text"
                  placeholder="key: value"
                  data-testid="issue-tag-input"
                  class="w-36 rounded border border-line bg-input px-2 py-0.5 text-xs text-fg placeholder:text-faint focus:border-accent focus:outline-none"
                />
                <button
                  type="submit"
                  class="btn-secondary px-2 py-0.5 text-xs"
                  data-testid="issue-tag-add"
                  :disabled="busy[i.id]"
                >
                  Add
                </button>
              </form>
            </div>
          </div>

          <div class="flex items-center gap-2">
            <button
              type="button"
              class="btn-primary px-3 py-1 text-xs"
              data-testid="issue-save"
              :disabled="busy[i.id]"
              @click="saveEdit(i)"
            >
              Save
            </button>
            <button
              type="button"
              class="btn-secondary px-3 py-1 text-xs"
              :disabled="busy[i.id]"
              @click="editing = null"
            >
              Cancel
            </button>
          </div>
        </div>
      </li>
    </ul>

    <nav
      v-if="visible.length > PAGE_SIZE"
      class="mt-3 flex items-center justify-between text-xs text-muted"
      aria-label="Issue pages"
      data-testid="issues-pagination"
    >
      <span>
        {{ (page - 1) * PAGE_SIZE + 1 }}–{{ Math.min(page * PAGE_SIZE, visible.length) }} of
        {{ visible.length }}
      </span>
      <span class="flex items-center gap-2">
        <button
          type="button"
          class="btn-secondary px-2 py-1"
          data-testid="issues-page-previous"
          :disabled="page === 1"
          @click="page--"
        >
          Previous
        </button>
        <span class="font-mono">Page {{ page }} / {{ pageCount }}</span>
        <button
          type="button"
          class="btn-secondary px-2 py-1"
          data-testid="issues-page-next"
          :disabled="page === pageCount"
          @click="page++"
        >
          Next
        </button>
      </span>
    </nav>

    <ConfirmDialog
      :open="deleteRequest !== null"
      :title="
        deleteRequest?.single
          ? `Delete issue #${deleteRequest.single.id}?`
          : 'Delete selected issues?'
      "
      :description="
        deleteRequest?.single
          ? `Permanently delete “${deleteRequest.single.title}”. This cannot be undone.`
          : `Permanently delete ${deleteRequest?.ids.length ?? 0} selected issues${
              hasScope ? ' in the active scope' : ''
            }. This cannot be undone.`
      "
      confirm-label="Delete permanently"
      danger
      :busy="batchBusy || Boolean(deleteRequest?.single && busy[deleteRequest.single.id])"
      @cancel="deleteRequest = null"
      @confirm="confirmDelete"
    />

    <ConfirmDialog
      :open="tagDialogOpen"
      title="Update tags"
      :description="`Apply one tag action atomically to ${selectedCount} selected issue${
        selectedCount === 1 ? '' : 's'
      }${hasScope ? ' in the active scope' : ''}.`"
      :confirm-label="tagMode === 'tag' ? 'Apply tag' : 'Remove tag'"
      :busy="batchBusy"
      @cancel="tagDialogOpen = false"
      @confirm="confirmTag"
    >
      <div class="space-y-3">
        <label class="block text-sm text-muted">
          Action
          <select
            v-model="tagMode"
            class="mt-1 w-full rounded border border-line bg-input px-2 py-1.5 text-fg"
            data-testid="issues-tag-mode"
          >
            <option value="tag">Set tag</option>
            <option value="untag">Remove tag</option>
          </select>
        </label>
        <label class="block text-sm text-muted">
          Key
          <input
            v-model="bulkTagKey"
            class="mt-1 w-full rounded border border-line bg-input px-2 py-1.5 text-fg"
            data-testid="issues-bulk-tag-key"
          />
        </label>
        <label v-if="tagMode === 'tag'" class="block text-sm text-muted">
          Value
          <input
            v-model="bulkTagValue"
            class="mt-1 w-full rounded border border-line bg-input px-2 py-1.5 text-fg"
            data-testid="issues-bulk-tag-value"
          />
        </label>
        <p v-if="bulkTagError" role="alert" class="text-sm text-block">{{ bulkTagError }}</p>
      </div>
    </ConfirmDialog>
  </div>
</template>
