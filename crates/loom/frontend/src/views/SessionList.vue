<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { get, post } from '../api';
import type { Session, RecentRepo, RepoBranch } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import SignalChip from '../components/SignalChip.vue';
import IdleChip from '../components/IdleChip.vue';
import TagPill from '../components/TagPill.vue';
import GithubStatus from '../components/GithubStatus.vue';
import ScratchPicker from '../components/ScratchPicker.vue';
import { timeAgo } from '../lib/time';
import { effectiveAttention, idleTag, messageOf, quietTags, signalChips } from '../lib/sessionState';
import { del } from '../api';

const sessions = ref<Session[]>([]);

// Attention filter — the dashboard's "which sessions need me?" control. The
// URL query is the source of truth (`/?filter=attention`, the status bar's
// deep-link): the buttons write it via router.replace and the ref follows, so
// the view is shareable and survives reload/back-forward.
type AttentionFilter = 'all' | 'attention' | 'ok';
const route = useRoute();
const router = useRouter();
function filterFromQuery(q: unknown): AttentionFilter {
  return q === 'attention' || q === 'ok' ? q : 'all';
}
const filter = ref<AttentionFilter>(filterFromQuery(route.query.filter));
// The component is reused when only the query changes (status-bar click while
// already on the fleet list), so track it.
watch(
  () => route.query.filter,
  (q) => (filter.value = filterFromQuery(q)),
);
// Button clicks update the query (replace, not push — filter flips shouldn't
// pollute history); the watcher above folds it back into the ref.
function setFilter(f: AttentionFilter) {
  filter.value = f;
  router.replace({ query: { ...route.query, filter: f === 'all' ? undefined : f } });
}

// Archived sessions are torn-down workstreams: kept for reference but clutter
// the live fleet view. Hide them by default; a reveal chip brings them back.
// They still show when there's nothing else to look at (an all-archived fleet),
// so the list never reads as empty while archived rows exist.
const showArchived = ref(false);

const archivedCount = computed(
  () => sessions.value.filter((s) => s.status === 'archived').length,
);

// The archived-aware, filtered set to display (membership only — the tree below
// imposes the order). Archived rows are hidden by default; a reveal chip brings
// them back, and they always show when there's nothing else, so the list never
// reads empty while archived rows exist. Individual attention rows aren't pinned
// to the top — threading keeps related work grouped — but a whole thread that
// contains attention floats up (see treeRows), and attention rows carry their
// loud signal chip, so they stay easy to spot.
const visibleSessions = computed<Session[]>(() => {
  const all = sessions.value;
  const live = all.filter((s) => s.status !== 'archived');
  const base = showArchived.value || live.length === 0 ? all : live;
  if (filter.value === 'attention') return base.filter((s) => effectiveAttention(s).level !== 'ok');
  if (filter.value === 'ok') return base.filter((s) => effectiveAttention(s).level === 'ok');
  return base;
});

// Counts reflect the full fleet (NOT the archived-hidden view) so the filter
// chips read the true picture; effectiveAttention() already forces archived → ok
// and ignores stale overlooker marks, keeping "needs attention" honest.
const counts = computed(() => {
  const c = { all: sessions.value.length, attention: 0, ok: 0 };
  for (const s of sessions.value) {
    if (effectiveAttention(s).level === 'ok') c.ok += 1;
    else c.attention += 1; // 'attention' and 'blocked' both need a human
  }
  return c;
});

// One flattened tree row: the session, its depth, and the gutter guides drawn to
// its left. `verticals[i]` is true when an ancestor's line keeps running down
// past this row; `isLast` picks the connector (└ vs ├). Depth 0 (top-level)
// draws no gutter, so a flat fleet with no sub-sessions looks exactly as before.
interface TreeRow {
  session: Session;
  depth: number;
  verticals: boolean[];
  isLast: boolean;
}

// Group the visible sessions into threads: each hangs under the session that
// launched it (`parent_id`, a branch id), with top-level sessions under an
// implicit root. A parent always sits directly above its children. Threads stay
// grouped, but a thread containing attention floats to the top: roots and
// siblings sort by their subtree's urgency first (blocked above attention above
// ok), then newest-first within the same urgency — so a blocked/attention child
// can never sink to the bottom of the fleet. A parent that's filtered/archived
// out of the visible set (or was never tracked) drops its orphaned children up.
const treeRows = computed<TreeRow[]>(() => {
  const list = visibleSessions.value;
  const present = new Set(list.map((s) => s.branch.id));
  const children = new Map<string, Session[]>();
  const roots: Session[] = [];
  for (const s of list) {
    const pid = s.parent_id;
    if (pid && pid !== s.branch.id && present.has(pid)) {
      const arr = children.get(pid);
      if (arr) arr.push(s);
      else children.set(pid, [s]);
    } else {
      roots.push(s);
    }
  }
  // Attention rank for a single session: blocked is louder than attention is
  // louder than ok. Used to float urgent threads to the top. Uses the resolved
  // signal (agent's own report or a non-stale overlooker mark) so a triaged
  // thread floats too — matching visibleSessions/counts above.
  const rankOf = (s: Session): number => {
    const lvl = effectiveAttention(s).level;
    return lvl === 'blocked' ? 2 : lvl === 'attention' ? 1 : 0;
  };
  // Memoized max attention rank across a session's whole subtree (itself + all
  // descendants), so a thread surfaces at the urgency of its loudest member.
  // The cycle guard mirrors walk()'s: a parent link loop can't spin us forever.
  const rankCache = new Map<string, number>();
  const ranking = new Set<string>();
  const subtreeRank = (s: Session): number => {
    const cached = rankCache.get(s.branch.id);
    if (cached !== undefined) return cached;
    if (ranking.has(s.branch.id)) return rankOf(s); // mid-cycle: just self
    ranking.add(s.branch.id);
    let max = rankOf(s);
    for (const kid of children.get(s.branch.id) ?? []) {
      max = Math.max(max, subtreeRank(kid));
    }
    ranking.delete(s.branch.id);
    rankCache.set(s.branch.id, max);
    return max;
  };
  // Newest-first within a sibling group, matching the dashboard's default feel.
  const byNewest = (a: Session, b: Session) =>
    b.created_at < a.created_at ? -1 : b.created_at > a.created_at ? 1 : 0;
  // Urgent subtree first, then newest-first as the tie-break. When no session
  // carries attention every rank is 0 and this collapses to plain byNewest.
  const byUrgencyThenNewest = (a: Session, b: Session) =>
    subtreeRank(b) - subtreeRank(a) || byNewest(a, b);

  const rows: TreeRow[] = [];
  const seen = new Set<string>(); // guard against any cycle in the parent links
  const walk = (node: Session, depth: number, verticals: boolean[], isLast: boolean) => {
    if (seen.has(node.branch.id)) return;
    seen.add(node.branch.id);
    rows.push({ session: node, depth, verticals, isLast });
    const kids = [...(children.get(node.branch.id) ?? [])].sort(byUrgencyThenNewest);
    kids.forEach((kid, i) => {
      const last = i === kids.length - 1;
      // The implicit root isn't a drawn column, so a top-level node's children
      // start with no ancestor lines; deeper, append this node's continuation.
      const childVerticals = depth === 0 ? [] : [...verticals, !isLast];
      walk(kid, depth + 1, childVerticals, last);
    });
  };
  const sortedRoots = [...roots].sort(byUrgencyThenNewest);
  sortedRoots.forEach((r, i) => walk(r, 0, [], i === sortedRoots.length - 1));
  return rows;
});
const recentRepos = ref<RecentRepo[]>([]);
const error = ref('');
const showForm = ref(false);
const repo = ref('');
const repoFocused = ref(false);
const title = ref('');
const goal = ref('');
const model = ref('');
const effort = ref('');
const name = ref('');
const nameEdited = ref(false);
// Optional base/parent branch to fork a new session from. Blank means the
// server default: a freshly-fetched `origin/<default branch>` (the mainline).
const base = ref('');
const creating = ref(false);
// Reference files staged in the form; base64-encoded into the create request
// so they land in the new worktree's scratch/ before the agent launches.
const scratchFiles = ref<File[]>([]);

// Read a File as base64 (JSON can't carry raw binary). Chunked so large files
// don't blow the argument limit of String.fromCharCode(...).
async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

type BranchMode = 'new' | 'existing';
const branchMode = ref<BranchMode>('new');
const existingBranch = ref('');
const branchFocused = ref(false);
const branches = ref<RepoBranch[]>([]);
const branchesError = ref('');
let branchesReqId = 0;
let timer: number | undefined;

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 40);
}

// Final path segment of a repo root, used as its short label in the dropdown.
function repoName(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() || path;
}

// Recently-used repos, narrowed to those matching what the user has typed.
const repoMatches = computed(() => {
  const q = repo.value.trim().toLowerCase();
  return recentRepos.value.filter((r) => r.repo_root.toLowerCase().includes(q));
});

const branchMatches = computed(() => {
  const q = existingBranch.value.trim().toLowerCase();
  if (!q) return branches.value;
  return branches.value.filter((b) => b.name.toLowerCase().includes(q));
});

function pickRepo(path: string) {
  repo.value = path;
  repoFocused.value = false;
}

function pickBranch(b: RepoBranch) {
  existingBranch.value = b.name;
  branchFocused.value = false;
}

// Keep the name in sync with the title (or goal) until the user edits it.
watch([title, goal], ([t, g]) => {
  if (!nameEdited.value) name.value = slugify(t || g);
});

async function loadBranches() {
  const path = repo.value.trim();
  branches.value = [];
  branchesError.value = '';
  if (!path) return;
  const reqId = ++branchesReqId;
  try {
    const res = (await get(`/repos/branches?cwd=${encodeURIComponent(path)}`)) as RepoBranch[];
    if (reqId === branchesReqId) branches.value = res;
  } catch (e) {
    if (reqId === branchesReqId) branchesError.value = (e as Error).message;
  }
}

// Fetch branches when the user switches into "existing branch" mode or
// changes the repo path while in that mode.
watch([repo, branchMode], ([, mode]) => {
  if (mode === 'existing') loadBranches();
});

async function load() {
  try {
    sessions.value = (await get('/sessions')) as Session[];
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

// A quiet pill's × clears that tag, then refreshes the row. The tag write
// surface is the same DELETE the detail page uses.
const clearingTag = ref('');
async function clearTag(sessionId: string, key: string) {
  clearingTag.value = `${sessionId}:${key}`;
  try {
    await del(`/sessions/${sessionId}/tags/${encodeURIComponent(key)}`);
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    clearingTag.value = '';
  }
}

async function loadRecentRepos() {
  try {
    recentRepos.value = (await get('/repos/recent')) as RecentRepo[];
  } catch {
    // The recent-repos dropdown is a convenience; ignore failures here.
  }
}

// Clear the form back to its initial state and hide it. Shared by the create()
// success path and the Cancel button so the two can't drift apart. The repo path
// is intentionally left as-is (kept out of create()'s reset) — it's the most
// reused field across sessions, so a fresh form pre-keeps the last repo typed.
function resetForm() {
  title.value = '';
  goal.value = '';
  model.value = '';
  effort.value = '';
  name.value = '';
  base.value = '';
  existingBranch.value = '';
  scratchFiles.value = [];
  nameEdited.value = false;
  branchMode.value = 'new';
  showForm.value = false;
}

async function create() {
  // A session needs a repo and at least a title or a goal; the goal alone is
  // optional (an empty goal just starts the agent unprompted).
  if (!repo.value.trim() || !(title.value.trim() || goal.value.trim())) return;
  if (branchMode.value === 'existing' && !existingBranch.value.trim()) return;
  creating.value = true;
  try {
    const body: Record<string, unknown> = {
      cwd: repo.value,
      title: title.value || undefined,
      goal: goal.value,
      model: model.value || undefined,
      effort: effort.value || undefined,
    };
    if (branchMode.value === 'existing') {
      body.existing_branch = existingBranch.value.trim();
    } else {
      body.name = name.value || undefined;
      // Only a new branch has a base to fork from; blank = server default.
      if (base.value.trim()) body.base = base.value.trim();
    }
    if (scratchFiles.value.length) {
      body.scratch = await Promise.all(
        scratchFiles.value.map(async (f) => ({
          name: f.name,
          content_base64: await fileToBase64(f),
        })),
      );
    }
    await post('/sessions', body);
    resetForm();
    await load();
    await loadRecentRepos();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

onMounted(() => {
  load();
  loadRecentRepos();
  timer = window.setInterval(load, 3000);
});
onUnmounted(() => clearInterval(timer));
</script>

<template>
  <div class="px-5 py-3">
    <!-- One toolbar line: the view label, the attention filter, the archived
         reveal, and the primary action — no page-hero heading (the rail
         already says where you are; the h1 stays for a11y + tests). -->
    <div class="mb-3 flex min-h-7 flex-wrap items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Sessions</h1>

      <!-- Attention filter: jump straight to the sessions that need a human.
           Each segment pairs a label with its count in a small pill so the
           number reads as a count, not a suffix glued to the word. -->
      <div v-if="sessions.length" class="inline-flex overflow-hidden rounded border border-line text-xs">
        <button
          v-for="opt in (['all', 'attention', 'ok'] as const)"
          :key="opt"
          type="button"
          :data-testid="`filter-${opt}`"
          :class="[
            'flex items-center gap-1.5 border-l border-line px-2.5 py-1 font-medium transition-colors first:border-l-0',
            filter === opt
              ? 'bg-accent text-accent-fg'
              : 'bg-input text-muted hover:bg-subtle hover:text-fg',
          ]"
          @click="setFilter(opt)"
        >
          {{ opt === 'all' ? 'All' : opt === 'attention' ? 'Needs attention' : 'OK' }}
          <span
            :class="[
              'rounded-full px-1.5 text-2xs leading-4',
              filter === opt ? 'bg-accent-fg/20 text-accent-fg' : 'bg-subtle text-faint',
            ]"
          >{{ counts[opt] }}</span>
        </button>
      </div>

      <!-- Archived live below the fold: a quiet chip reveals/hides them. -->
      <button
        v-if="archivedCount"
        type="button"
        :aria-pressed="showArchived"
        :class="[
          'rounded border border-line px-2.5 py-1 text-xs text-muted transition-colors',
          showArchived ? 'bg-subtle text-fg' : 'bg-input hover:bg-subtle',
        ]"
        @click="showArchived = !showArchived"
      >
        {{ showArchived ? 'Hide' : 'Show' }} {{ archivedCount }} archived
      </button>

      <!-- Toggles the create form. Closed → primary (accent) call-to-action;
           open → a neutral "Cancel" so it never reads as a second primary
           action competing with the form's own Create button. -->
      <button
        :class="[
          'ml-auto px-2.5 py-1 text-xs font-medium',
          showForm ? 'btn-secondary' : 'btn-primary',
        ]"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New session' }}
      </button>
    </div>

    <!--
      Grouped into labeled sections so the ~9-field form scans instead of
      reading as one flat stack: Repository, What to build, Agent, Branch, and
      Scratch files. The treatment is deliberately light — a small uppercase
      section label and a hairline top divider per group, no heavy boxes — so it
      stays consistent with the rest of the app's quiet surfaces.
    -->
    <form
      v-if="showForm"
      class="mb-4 max-w-3xl rounded-md border border-line bg-surface p-4 space-y-5"
      autocomplete="off"
      @submit.prevent="create"
    >
      <!-- Repository: where the work lives, with a recent-repos shortcut. -->
      <section class="space-y-3">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Repository</h2>
        <div class="relative">
          <label class="block text-xs text-muted mb-1">
            Repository path (on the server)
            <span v-if="recentRepos.length" class="text-faint">— or pick a recent one</span>
          </label>
          <input
            v-model="repo"
            @focus="repoFocused = true"
            @input="repoFocused = true"
            @blur="repoFocused = false"
            placeholder="/home/you/code/project"
            autocomplete="off"
            spellcheck="false"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
          />
          <ul
            v-if="repoFocused && repoMatches.length"
            data-testid="recent-repos"
            class="absolute left-0 right-0 z-10 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
          >
            <li v-for="r in repoMatches" :key="r.repo_root">
              <button
                type="button"
                data-testid="recent-repo"
                @mousedown.prevent="pickRepo(r.repo_root)"
                class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-subtle"
              >
                <span class="min-w-0">
                  <span class="block truncate text-sm">{{ repoName(r.repo_root) }}</span>
                  <span class="block truncate text-xs text-muted font-mono">{{ r.repo_root }}</span>
                </span>
                <span
                  v-if="r.active_branches"
                  :title="`${r.active_branches} tracked branch(es)`"
                  class="shrink-0 rounded bg-subtle px-1.5 py-0.5 text-xs text-muted"
                >
                  {{ r.active_branches }}
                </span>
              </button>
            </li>
          </ul>
        </div>
      </section>

      <!-- What to build: the human-facing intent — a short title and the goal. -->
      <section class="space-y-3 border-t border-line pt-3">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">What to build</h2>
        <div>
          <label class="block text-xs text-muted mb-1">Title</label>
          <input
            v-model="title"
            placeholder="Health endpoint"
            autocomplete="off"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
          />
        </div>
        <div>
          <label class="block text-xs text-muted mb-1">
            Goal — optional; leave blank to start the agent with no prompt
          </label>
          <textarea
            v-model="goal"
            rows="4"
            placeholder="Add a /health endpoint that returns 200"
            autocomplete="off"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
          ></textarea>
        </div>
      </section>

      <!-- Agent: which Claude tier and how hard it reasons. -->
      <section class="space-y-3 border-t border-line pt-3">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Agent</h2>
        <div class="grid grid-cols-2 gap-3">
          <div>
            <label class="block text-xs text-muted mb-1">Model</label>
            <select
              v-model="model"
              autocomplete="off"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            >
              <option value="">Default</option>
              <option value="haiku">Haiku</option>
              <option value="sonnet">Sonnet</option>
              <option value="opus">Opus</option>
              <option value="fable">Fable</option>
            </select>
          </div>
          <div>
            <label class="block text-xs text-muted mb-1">Effort</label>
            <select
              v-model="effort"
              autocomplete="off"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            >
              <option value="">Default</option>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="xhigh">X-High</option>
              <option value="max">Max</option>
            </select>
          </div>
          <p class="col-span-2 -mt-1 text-xs text-faint">
            Model tier and reasoning effort for the Claude agent. Leave as Default
            to inherit the configured launch args.
          </p>
        </div>
      </section>

      <!-- Branch: fork a fresh branch or reuse an existing one. -->
      <section class="space-y-3 border-t border-line pt-3">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Branch</h2>
        <div>
          <div class="inline-flex rounded border border-line text-xs overflow-hidden mb-2">
            <button
              type="button"
              :class="[
                'px-3 py-1',
                branchMode === 'new' ? 'bg-accent text-white' : 'bg-input text-muted hover:bg-subtle',
              ]"
              @click="branchMode = 'new'"
            >
              New branch
            </button>
            <button
              type="button"
              :class="[
                'px-3 py-1 border-l border-line',
                branchMode === 'existing' ? 'bg-accent text-white' : 'bg-input text-muted hover:bg-subtle',
              ]"
              @click="branchMode = 'existing'"
            >
              Existing branch
            </button>
          </div>
          <div v-if="branchMode === 'new'" class="space-y-2">
            <div>
              <label class="block text-xs text-muted mb-1">
                Name — the worktree (<code>.worktrees/&lt;name&gt;</code>) and branch
                (<code>weaver/&lt;name&gt;</code>)
              </label>
              <input
                v-model="name"
                @input="nameEdited = true"
                placeholder="health-endpoint"
                autocomplete="off"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
              />
            </div>
            <div>
              <label class="block text-xs text-muted mb-1">
                Base branch — fork point (optional)
              </label>
              <input
                v-model="base"
                placeholder="origin/main (freshly fetched)"
                autocomplete="off"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
              />
              <p class="mt-1 text-xs text-faint">
                Leave blank to fork from a freshly-fetched
                <code>origin/&lt;default branch&gt;</code>.
              </p>
            </div>
          </div>
          <div v-else class="relative">
            <label class="block text-xs text-muted mb-1">
              Existing branch — weaver reuses its worktree if one is checked out
            </label>
            <input
              v-model="existingBranch"
              @focus="branchFocused = true"
              @input="branchFocused = true"
              @blur="branchFocused = false"
              placeholder="feature/foo"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
            <p v-if="branchesError" class="mt-1 text-xs text-block">{{ branchesError }}</p>
            <ul
              v-if="branchFocused && branchMatches.length"
              data-testid="branch-options"
              class="absolute left-0 right-0 z-10 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
            >
              <li v-for="b in branchMatches" :key="b.name">
                <button
                  type="button"
                  data-testid="branch-option"
                  @mousedown.prevent="pickBranch(b)"
                  class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-subtle"
                >
                  <span class="min-w-0">
                    <span class="block truncate text-sm font-mono">
                      {{ b.name }}
                      <span v-if="b.current" class="ml-1 text-xs text-accent">(current)</span>
                    </span>
                    <span
                      v-if="b.worktree"
                      class="block truncate text-xs text-muted font-mono"
                    >→ {{ b.worktree }}</span>
                  </span>
                </button>
              </li>
            </ul>
          </div>
        </div>
      </section>

      <!-- Scratch files: reference material staged into the new worktree. -->
      <section class="space-y-3 border-t border-line pt-3">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Scratch files</h2>
        <ScratchPicker v-model="scratchFiles" />
      </section>

      <!-- Action row: Create leads (primary), Cancel discards + closes the form. -->
      <div class="flex items-center gap-2 border-t border-line pt-3">
        <button
          type="submit"
          :disabled="creating"
          class="btn-primary px-3 py-1.5 text-sm font-medium"
        >
          {{ creating ? 'Creating…' : 'Create' }}
        </button>
        <button
          type="button"
          class="btn-secondary px-3 py-1.5 text-sm font-medium"
          @click="resetForm"
        >
          Cancel
        </button>
      </div>
    </form>

    <p v-if="error" class="mb-4 text-sm text-block">{{ error }}</p>

    <div
      v-if="!sessions.length"
      class="rounded-md border border-dashed border-line p-6 text-center"
    >
      <p class="text-sm text-muted">No sessions yet.</p>
      <p class="mt-1 text-xs text-faint">
        Launch one with <strong>New session</strong>, or
        <code>loom session launch "&lt;goal&gt;"</code>.
      </p>
    </div>

    <!--
      One row per session. Left→right: an optional tree gutter threading child
      sessions under their launcher, the dominant title with its loud signal
      chips (attention/triage, each deletable) and quiet tag pills alongside, a
      muted current-state line, a neutral lifecycle pill (shown only for
      off-nominal states — running is the silent default), and the mono branch
      ref pushed far-right. Rows are grouped into threads (built in script), with
      attention-carrying threads floated up so the loud chips surface near the
      top. The row itself stays neutral — no full-tile wash — so threading reads
      cleanly; the chip carries the signal. Stagger via --i.
    -->
    <ul v-if="sessions.length" data-testid="session-list" class="overflow-hidden rounded-md border border-line bg-surface">
      <li
        v-for="({ session: s, depth, verticals, isLast }, i) in treeRows"
        :key="s.id"
        data-testid="session-card"
        :data-session-id="s.id"
        :data-depth="depth"
        :style="{ '--i': i }"
        :class="[
          'stagger-in group flex cursor-pointer items-start gap-2.5 border-b border-line px-3 py-2 last:border-0',
          'min-h-11 transition-colors hover:bg-subtle',
        ]"
        @click="$router.push(`/s/${s.id}`)"
      >
        <!-- Tree gutter: threads a child session under the one that launched it.
             Drawn only for nested rows, so a flat fleet is visually unchanged. -->
        <div v-if="depth > 0" class="tree-gutter" aria-hidden="true">
          <span
            v-for="(v, c) in verticals"
            :key="c"
            class="tree-col"
            :class="{ 'tree-col--through': v }"
          ></span>
          <span class="tree-col" :class="isLast ? 'tree-col--elbow' : 'tree-col--tee'"></span>
        </div>

        <!-- Title + current-state (the work, in prose). -->
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <router-link
              :to="`/s/${s.id}`"
              class="truncate text-sm font-semibold text-fg hover:text-accent"
              @click.stop
            >
              {{ s.branch.title || s.branch.name }}
            </router-link>
            <!-- Loud signals: the agent's `attention` and an overlooker's
                 `triage`, each a deletable chip. The × clears that tag (calm is
                 its absence) — there is no separate "Mark OK" verb. -->
            <SignalChip
              v-for="chip in signalChips(s)"
              :key="chip.key"
              :chip="chip"
              :busy="clearingTag === `${s.id}:${chip.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
            <!-- Lifecycle: demoted, neutral, mono pill (StatusBadge). Hidden for
                 the running state — nearly every live row is running, so the pill
                 would just be repeated noise; only off-nominal states show one. -->
            <StatusBadge v-if="s.status !== 'running'" :status="s.status" class="shrink-0" />
            <!-- Soothing idle mark: a calm, neutral chip when the agent is
                 resting (no loud signal). Reassures rather than alarms. -->
            <IdleChip v-if="idleTag(s)" :tag="idleTag(s)!" />
            <!-- Quiet free-form tags: deletable pills, never the loud fill. -->
            <TagPill
              v-for="t in quietTags(s)"
              :key="t.key"
              :tag="t"
              :busy="clearingTag === `${s.id}:${t.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
          </div>

          <!-- Current-state headline (agent's status message), else the goal.
               On an attention row the goal steps up from faint to muted so the
               metadata doesn't recede next to the loud chip. -->
          <p
            v-if="messageOf(s)"
            class="mt-0.5 truncate text-xs text-muted"
          >
            {{ messageOf(s) }}
          </p>
          <p
            v-if="s.branch.goal"
            class="mt-0.5 truncate text-xs"
            :class="effectiveAttention(s).level === 'ok' ? 'text-faint' : 'text-muted'"
          >
            {{ s.branch.goal }}
          </p>
        </div>

        <!-- Ref: machine identity, mono, pushed far-right and receding. -->
        <div class="shrink-0 text-right">
          <span class="block truncate font-mono text-2xs text-faint">{{ s.branch.branch }}</span>
          <!-- PR snapshot (if any) — a quiet link straight to the GitHub PR. -->
          <GithubStatus v-if="s.branch.github" :gh="s.branch.github" compact class="mt-0.5 justify-end" />
          <router-link
            v-if="s.branch.open_issue_count"
            :to="`/s/${s.id}?tab=overview`"
            class="block font-mono text-2xs text-muted hover:text-accent"
            @click.stop
          >
            {{ s.branch.open_issue_count }} open issue{{ s.branch.open_issue_count === 1 ? '' : 's' }}
          </router-link>
          <span v-if="s.last_activity_at" class="mt-0.5 block font-mono text-2xs text-faint">
            {{ timeAgo(s.last_activity_at) }}
          </span>
        </div>
      </li>
    </ul>
  </div>
</template>
