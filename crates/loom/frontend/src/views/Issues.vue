<script setup lang="ts">
import { ref, reactive, computed, nextTick, onMounted, onActivated } from 'vue';
import { useRouter } from 'vue-router';
import {
  get,
  listIssues,
  createRepoIssue,
  patchIssue,
  deleteIssue,
  setIssueTag,
  clearIssueTag,
  launchSessionForIssue,
} from '../api';
import type { Issue, Session, Tag } from '../types';
import TagPill from '../components/TagPill.vue';
import { timeAgo } from '../lib/time';

// Named so App.vue's <keep-alive :include> keeps this view warm across nav.
defineOptions({ name: 'Issues' });

const router = useRouter();

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
const search = ref('');
const repoFilter = ref('');

// Per-issue UI state: which row's editor is open, the edit draft, the per-row
// new-tag input, and a busy flag that disables a row's controls mid-call.
const editing = ref<number | null>(null);
const draft = reactive<{ title: string; body: string }>({ title: '', body: '' });
const newTag = reactive<Record<number, string>>({});
const busy = reactive<Record<number, boolean>>({});

async function load() {
  try {
    // Fetch everything (including closed issues / archived sessions) once;
    // `showClosed` filters client-side, and archived sessions still reference
    // their issues. The API hides archived by default, so ask for them.
    const [iss, ses] = await Promise.all([
      listIssues(true),
      get('/sessions?archived=true') as Promise<Session[]>,
    ]);
    issues.value = iss;
    sessions.value = ses;
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
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
// That endpoint takes no tags, so any tags staged here are applied as follow-up
// `setIssueTag` upserts on the returned id.
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
  for (const i of issues.value) set.add(i.repo_root);
  for (const s of sessions.value) set.add(s.branch.repo_root);
  return [...set].sort();
});

async function openCreate() {
  showCreate.value = true;
  createError.value = '';
  // Default to the active repo filter, else the first known repo.
  createRepo.value = repoFilter.value || repoChoices.value[0] || '';
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
    let created = await createRepoIssue(repo, title, createDraft.body);
    // Apply staged tags as upserts; each call returns the updated issue, so the
    // last response carries the fully-tagged row we insert into the list.
    for (const t of createTags.value) {
      created = await setIssueTag(created.id, t.key, t.value);
    }
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

const visible = computed(() => {
  const q = search.value.trim().toLowerCase();
  return issues.value.filter((i) => {
    if (!showClosed.value && i.status !== 'open') return false;
    if (repoFilter.value && i.repo_root !== repoFilter.value) return false;
    if (!q) return true;
    const hay = [
      `#${i.id}`,
      i.title,
      i.body,
      ...i.tags.map((t) => `${t.key} ${t.value}`),
    ]
      .join(' ')
      .toLowerCase();
    return hay.includes(q);
  });
});

const openCount = computed(() => issues.value.filter((i) => i.status === 'open').length);

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

async function remove(i: Issue) {
  if (!confirm(`Delete issue #${i.id} "${i.title}"? This cannot be undone.`)) return;
  await withBusy(i.id, async () => {
    await deleteIssue(i.id);
    issues.value = issues.value.filter((x) => x.id !== i.id);
    if (editing.value === i.id) editing.value = null;
  });
}

function startEdit(i: Issue) {
  if (editing.value === i.id) {
    editing.value = null;
    return;
  }
  editing.value = i.id;
  draft.title = i.title;
  draft.body = i.body;
}

async function saveEdit(i: Issue) {
  const title = draft.title.trim();
  if (!title) {
    error.value = 'issue title is required';
    return;
  }
  await withBusy(i.id, async () => {
    replaceIssue((await patchIssue(i.id, { title, body: draft.body })) as Issue);
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

      <div class="ml-auto flex flex-wrap items-center gap-2">
        <input
          v-model="search"
          type="search"
          placeholder="Filter issues…"
          data-testid="issues-search"
          class="w-48 rounded bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        />
        <select
          v-if="multiRepo"
          v-model="repoFilter"
          data-testid="issues-repo-filter"
          class="rounded bg-input px-2 py-1 text-xs text-fg outline-none ring-accent focus:ring-1"
        >
          <option value="">All repos</option>
          <option v-for="r in repos" :key="r" :value="r">{{ repoName(r) }}</option>
        </select>
        <label class="flex items-center gap-1.5 text-xs text-muted">
          <input v-model="showClosed" type="checkbox" class="accent-accent" data-testid="issues-show-closed" />
          Show closed
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
      unclaimed backlog item; staged tags apply as follow-up upserts.
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
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted">Repository</span>
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
        >{{ repoName(createRepo) }}</p>
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
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted">Title</span>
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
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted">Body</span>
        <textarea
          v-model="createDraft.body"
          rows="4"
          placeholder="Optional detail, acceptance criteria, links…"
          data-testid="issue-create-body"
          class="w-full rounded bg-input px-2 py-1 font-mono text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
        ></textarea>
      </label>

      <div>
        <span class="mb-1 block text-2xs font-semibold uppercase tracking-wider text-muted">Tags</span>
        <div class="flex flex-wrap items-center gap-1.5">
          <TagPill
            v-for="t in createTags"
            :key="t.key"
            :tag="t"
            @clear="removeCreateTag"
          />
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
            >Add</button>
          </span>
        </div>
      </div>

      <p v-if="createError" class="text-sm text-block" data-testid="issue-create-error">{{ createError }}</p>

      <div class="flex items-center gap-2">
        <button
          type="submit"
          class="btn-primary px-2.5 py-1 text-xs font-medium"
          data-testid="issue-create-submit"
          :disabled="creating"
        >{{ creating ? 'Creating…' : 'Create issue' }}</button>
        <button
          type="button"
          class="btn-secondary px-2.5 py-1 text-xs font-medium"
          :disabled="creating"
          @click="cancelCreate"
        >Cancel</button>
      </div>
    </form>

    <p v-if="error" class="mb-3 text-sm text-block" data-testid="issues-error">{{ error }}</p>

    <p v-if="!loaded" class="text-sm text-muted">Loading…</p>
    <p
      v-else-if="!visible.length"
      class="rounded-md border border-dashed border-line p-6 text-center text-sm text-faint"
      data-testid="issues-empty"
    >
      {{ issues.length ? 'No issues match the current filter.' : 'No issues yet.' }}
    </p>

    <!-- One bordered board, hairline-divided rows (the fleet-list anatomy).
         Per-row actions are ghost buttons revealed on hover/focus, so the
         board reads as data, not as a wall of buttons. -->
    <ul v-else class="overflow-hidden rounded-md border border-line bg-surface" data-testid="issues-list">
      <li
        v-for="i in visible"
        :key="i.id"
        class="group border-b border-line px-3 py-2 last:border-0 transition-colors hover:bg-subtle/50"
        :class="{ 'opacity-60': i.status !== 'open' }"
        data-testid="issue-row"
        :data-issue-id="i.id"
      >
        <!-- Row 1: status dot · id · title (click to edit) · repo chip ·
             actions (hover-revealed) · freshness -->
        <div class="flex items-center gap-2">
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
          <span
            v-if="multiRepo"
            class="pill shrink-0 font-mono"
            :title="i.repo_root"
          >{{ repoName(i.repo_root) }}</span>
          <a
            v-if="i.github_issue && i.github_repo"
            :href="`https://github.com/${i.github_repo}/issues/${i.github_issue}`"
            target="_blank"
            rel="noopener"
            class="shrink-0 font-mono text-2xs text-muted hover:text-accent"
            @click.stop
          >gh #{{ i.github_issue }}</a>

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
            >{{ launching === i.id ? 'Launching…' : 'Launch' }}</button>
            <button
              v-if="i.status === 'open'"
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-close"
              :disabled="busy[i.id]"
              @click="setStatus(i, 'closed')"
            >Close</button>
            <button
              v-else
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-reopen"
              :disabled="busy[i.id]"
              @click="setStatus(i, 'open')"
            >Reopen</button>
            <button
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-subtle hover:text-fg"
              data-testid="issue-edit"
              :disabled="busy[i.id]"
              @click="startEdit(i)"
            >{{ editing === i.id ? 'Cancel' : 'Edit' }}</button>
            <button
              type="button"
              class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-block-soft hover:text-block"
              data-testid="issue-delete"
              :disabled="busy[i.id]"
              @click="remove(i)"
            >Delete</button>
          </div>

          <span
            class="shrink-0 font-mono text-2xs text-faint"
            :title="`updated ${i.updated_at} · created ${i.created_at}`"
          >{{ timeAgo(i.updated_at) }}</span>
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
              >{{ r.rel }}: {{ r.session.branch.name }}</router-link>
            </template>
          </div>
          <span
            v-else-if="i.claimed_branch || i.source_branch"
            class="font-mono text-faint"
            data-testid="issue-branch-ref"
          >
            {{ i.claimed_branch ? `claimed: ${branchLabel(i.claimed_branch)}` : `from: ${branchLabel(i.source_branch!)}` }}
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
                >Add</button>
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
            >Save</button>
            <button
              type="button"
              class="btn-secondary px-3 py-1 text-xs"
              :disabled="busy[i.id]"
              @click="editing = null"
            >Cancel</button>
          </div>
        </div>
      </li>
    </ul>
  </div>
</template>
