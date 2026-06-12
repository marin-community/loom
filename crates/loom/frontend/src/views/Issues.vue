<script setup lang="ts">
import { ref, reactive, computed, onMounted } from 'vue';
import { get, listIssues, patchIssue, deleteIssue, setIssueTag, clearIssueTag } from '../api';
import type { Issue, Session } from '../types';
import TagPill from '../components/TagPill.vue';
import { timeAgo } from '../lib/time';

// The Issues pane — the cross-repo weaver issue board, sibling to the session
// list and the overlooker panel. API-first: every row is an `IssueView` from
// `GET /api/issues`, every control a REST call. Issues are repo-scoped data, so
// the whole fleet's issues land here and a repo chip / filter disambiguates when
// more than one repo is in play.
//
// What you can do per issue: click the title to edit (title + body), close /
// reopen, delete, and manage its free-form `(key, value)` tags as deletable
// pills. The sessions that reference an issue — the branch working it
// (`claimed`) and the branch it came from (`source`) — resolve to live session
// links from the session list.

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
    // Fetch everything (including closed) once; `showClosed` filters client-side.
    const [iss, ses] = await Promise.all([
      listIssues(true),
      get('/sessions') as Promise<Session[]>,
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

// The short repo label is the last path segment of the repo root.
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}

// Distinct repos present, for the repo filter and the per-row chip (shown only
// when the board spans more than one repo).
const repos = computed(() => [...new Set(issues.value.map((i) => i.repo_root))].sort());
const multiRepo = computed(() => repos.value.length > 1);

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

// Sessions that reference an issue: the branch working it (claimed) and the
// branch it came from (source), matched against the live session list by
// repo + branch name. Claimed first, deduped, each tagged with its relation.
function refsFor(i: Issue): { session: Session; rel: string }[] {
  const out: { session: Session; rel: string }[] = [];
  const seen = new Set<string>();
  const match = (branch: string | null, rel: string) => {
    if (!branch) return;
    for (const s of sessions.value) {
      if (s.branch.repo_root === i.repo_root && s.branch.branch === branch && !seen.has(s.id)) {
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
  <div>
    <div class="mb-4 flex flex-wrap items-center gap-3">
      <h1 class="text-lg font-semibold tracking-tight">Issues</h1>
      <span class="pill" data-testid="issues-open-count">{{ openCount }} open</span>

      <div class="ml-auto flex flex-wrap items-center gap-2">
        <input
          v-model="search"
          type="search"
          placeholder="Filter issues…"
          data-testid="issues-search"
          class="w-48 rounded border border-line bg-input px-2 py-1 text-sm text-fg placeholder:text-faint focus:border-accent focus:outline-none"
        />
        <select
          v-if="multiRepo"
          v-model="repoFilter"
          data-testid="issues-repo-filter"
          class="rounded border border-line bg-input px-2 py-1 text-sm text-fg focus:border-accent focus:outline-none"
        >
          <option value="">All repos</option>
          <option v-for="r in repos" :key="r" :value="r">{{ repoName(r) }}</option>
        </select>
        <label class="flex items-center gap-1.5 text-sm text-muted">
          <input v-model="showClosed" type="checkbox" data-testid="issues-show-closed" />
          Show closed
        </label>
      </div>
    </div>

    <p v-if="error" class="mb-3 text-sm text-block" data-testid="issues-error">{{ error }}</p>

    <p v-if="!loaded" class="text-muted">Loading…</p>
    <p
      v-else-if="!visible.length"
      class="rounded border border-line bg-surface p-6 text-center text-sm text-faint"
      data-testid="issues-empty"
    >
      {{ issues.length ? 'No issues match the current filter.' : 'No issues yet.' }}
    </p>

    <ul v-else class="space-y-2" data-testid="issues-list">
      <li
        v-for="i in visible"
        :key="i.id"
        class="rounded border border-line bg-surface p-3"
        :class="{ 'opacity-60': i.status !== 'open' }"
        data-testid="issue-row"
        :data-issue-id="i.id"
      >
        <!-- Row 1: id · title (click to edit) · repo chip · status -->
        <div class="flex items-start gap-2">
          <span class="mt-0.5 font-mono text-xs text-muted">#{{ i.id }}</span>
          <button
            type="button"
            class="min-w-0 flex-1 text-left text-sm text-fg hover:text-accent"
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
            class="shrink-0 text-xs text-muted hover:text-accent"
            @click.stop
          >gh #{{ i.github_issue }}</a>
          <span
            class="shrink-0 rounded px-2 py-0.5 text-[0.7rem] font-medium uppercase tracking-wide font-mono"
            :class="i.status === 'open' ? 'bg-accent/10 text-accent ring-1 ring-inset ring-accent/30' : 'bg-subtle text-faint'"
            data-testid="issue-status"
          >{{ i.status }}</span>
        </div>

        <!-- Row 2: tag pills + referencing sessions + freshness -->
        <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5 pl-7 text-xs">
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

          <span class="ml-auto text-faint" :title="i.created_at">{{ timeAgo(i.updated_at) }}</span>
        </div>

        <!-- Row 3: quick actions -->
        <div class="mt-2 flex items-center gap-2 pl-7">
          <button
            v-if="i.status === 'open'"
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            data-testid="issue-close"
            :disabled="busy[i.id]"
            @click="setStatus(i, 'closed')"
          >Close</button>
          <button
            v-else
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            data-testid="issue-reopen"
            :disabled="busy[i.id]"
            @click="setStatus(i, 'open')"
          >Reopen</button>
          <button
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            data-testid="issue-edit"
            :disabled="busy[i.id]"
            @click="startEdit(i)"
          >{{ editing === i.id ? 'Cancel' : 'Edit' }}</button>
          <button
            type="button"
            class="btn-danger ml-auto px-2 py-1 text-xs"
            data-testid="issue-delete"
            :disabled="busy[i.id]"
            @click="remove(i)"
          >Delete</button>
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
