<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue';
import { get, post } from '../api';
import type { Session, RecentRepo, RepoBranch } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import AttentionBadge from '../components/AttentionBadge.vue';
import ScratchPicker from '../components/ScratchPicker.vue';
import { timeAgo } from '../lib/time';
import { levelOf, messageOf } from '../lib/sessionState';

const sessions = ref<Session[]>([]);

// Attention filter — the dashboard's "which sessions need me?" control.
type AttentionFilter = 'all' | 'attention' | 'ok';
const filter = ref<AttentionFilter>('all');

// Archived sessions are torn-down workstreams: kept for reference but clutter
// the live fleet view. Hide them by default; a reveal chip brings them back.
// They still show when there's nothing else to look at (an all-archived fleet),
// so the list never reads as empty while archived rows exist.
const showArchived = ref(false);

const archivedCount = computed(
  () => sessions.value.filter((s) => s.status === 'archived').length,
);

// Attention rank: blocked and attention both "need a human" and pin to the top;
// ok sinks below. Within a rank, preserve the server order (most-recent-first),
// so the sort is stable and rows don't jitter between 3s polls.
function rank(s: Session): number {
  const lvl = levelOf(s);
  if (lvl === 'blocked') return 0;
  if (lvl === 'attention') return 1;
  return 2;
}

// The ordered, archived-aware base list: attention pinned to the top, archived
// hidden unless revealed (or unless every session is archived — see above).
const ordered = computed<Session[]>(() => {
  const all = sessions.value;
  const live = all.filter((s) => s.status !== 'archived');
  // Hide archived by default; reveal on toggle; always show if nothing else.
  const base = showArchived.value || live.length === 0 ? all : live;
  // Stable sort by attention rank (Array.prototype.sort is stable in modern JS).
  return [...base].sort((a, b) => rank(a) - rank(b));
});

// Counts reflect the full fleet (NOT the archived-hidden view) so the filter
// chips read the true picture; levelOf() already forces archived → ok, keeping
// "needs attention" honest.
const counts = computed(() => {
  const c = { all: sessions.value.length, attention: 0, ok: 0 };
  for (const s of sessions.value) {
    if (levelOf(s) === 'ok') c.ok += 1;
    else c.attention += 1; // 'attention' and 'blocked' both need a human
  }
  return c;
});

// Apply the attention filter on top of the ordered/archived-aware base list.
const filteredSessions = computed(() => {
  if (filter.value === 'all') return ordered.value;
  if (filter.value === 'ok') return ordered.value.filter((s) => levelOf(s) === 'ok');
  return ordered.value.filter((s) => levelOf(s) !== 'ok');
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

async function loadRecentRepos() {
  try {
    recentRepos.value = (await get('/repos/recent')) as RecentRepo[];
  } catch {
    // The recent-repos dropdown is a convenience; ignore failures here.
  }
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
    title.value = '';
    goal.value = '';
    model.value = '';
    effort.value = '';
    name.value = '';
    existingBranch.value = '';
    scratchFiles.value = [];
    nameEdited.value = false;
    branchMode.value = 'new';
    showForm.value = false;
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
  <div>
    <div class="flex items-center justify-between mb-4">
      <h1 class="text-xl font-semibold">Sessions</h1>
      <button
        class="rounded bg-accent hover:bg-accent-hover px-3 py-1.5 text-sm font-medium"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New session' }}
      </button>
    </div>

    <form
      v-if="showForm"
      class="mb-5 rounded border border-line bg-surface p-4 space-y-3"
      @submit.prevent="create"
    >
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
      <div>
        <label class="block text-xs text-muted mb-1">Title</label>
        <input
          v-model="title"
          placeholder="Health endpoint"
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
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
        ></textarea>
      </div>
      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="block text-xs text-muted mb-1">Model</label>
          <select
            v-model="model"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
          >
            <option value="">Default</option>
            <option value="haiku">Haiku</option>
            <option value="sonnet">Sonnet</option>
            <option value="opus">Opus</option>
          </select>
        </div>
        <div>
          <label class="block text-xs text-muted mb-1">Effort</label>
          <select
            v-model="effort"
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
        <div v-if="branchMode === 'new'">
          <label class="block text-xs text-muted mb-1">
            Name — the worktree (<code>.worktrees/&lt;name&gt;</code>) and branch
            (<code>weaver/&lt;name&gt;</code>)
          </label>
          <input
            v-model="name"
            @input="nameEdited = true"
            placeholder="health-endpoint"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
          />
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
      <ScratchPicker v-model="scratchFiles" />
      <button
        type="submit"
        :disabled="creating"
        class="rounded bg-accent hover:bg-accent-hover px-3 py-1.5 text-sm font-medium disabled:opacity-50"
      >
        {{ creating ? 'Creating…' : 'Create' }}
      </button>
    </form>

    <p v-if="error" class="mb-4 text-sm text-block">{{ error }}</p>

    <p v-if="!sessions.length" class="text-muted text-sm">
      No sessions yet.
    </p>

    <div v-if="sessions.length" class="mb-3 flex items-center gap-3">
      <!-- Attention filter: jump straight to the sessions that need a human. -->
      <div class="inline-flex rounded border border-line text-xs overflow-hidden">
        <button
          v-for="opt in (['all', 'attention', 'ok'] as const)"
          :key="opt"
          type="button"
          :data-testid="`filter-${opt}`"
          :class="[
            'px-3 py-1 border-l border-line first:border-l-0',
            filter === opt ? 'bg-accent text-accent-fg' : 'bg-input text-muted hover:bg-subtle',
          ]"
          @click="filter = opt"
        >
          {{ opt === 'all' ? 'All' : opt === 'attention' ? 'Needs attention' : 'OK' }}
          <span class="opacity-70">{{ counts[opt] }}</span>
        </button>
      </div>

      <!-- Archived live below the fold: a quiet chip reveals/hides them. -->
      <button
        v-if="archivedCount"
        type="button"
        :aria-pressed="showArchived"
        :class="[
          'pill transition-colors',
          showArchived ? 'ring-1 ring-inset ring-line text-fg' : 'hover:bg-subtle-hover',
        ]"
        @click="showArchived = !showArchived"
      >
        {{ showArchived ? 'Hide' : 'Show' }} {{ archivedCount }} archived
      </button>
    </div>

    <!--
      One signal row per session. Left→right: the agent's single attention
      signal, the dominant title, its muted current-state line, a neutral
      lifecycle pill, and the mono branch ref pushed far-right. Attention rows
      pin to the top (sort in script) and get a left accent-border + slow pulse;
      ok rows stay quiet. Staggered reveal on load via --i.
    -->
    <ul v-if="sessions.length" class="overflow-hidden rounded border border-line bg-surface">
      <li
        v-for="(s, i) in filteredSessions"
        :key="s.id"
        data-testid="session-card"
        :data-session-id="s.id"
        :style="{ '--i': i }"
        :class="[
          'stagger-in group flex cursor-pointer items-start gap-3 border-b border-line px-3 py-3 last:border-0',
          'min-h-[3.25rem] transition-colors hover:bg-subtle',
          levelOf(s) === 'blocked'
            ? 'border-l-2 border-l-block-line bg-block-soft pulse-attention'
            : levelOf(s) === 'attention'
              ? 'border-l-2 border-l-attn-line bg-attn-soft pulse-attention'
              : '',
        ]"
        @click="$router.push(`/s/${s.id}`)"
      >
        <!-- Signal: the one reserved loud axis. -->
        <div class="shrink-0 pt-0.5">
          <AttentionBadge :level="levelOf(s)" :note="messageOf(s)" />
        </div>

        <!-- Title + current-state (the work, in prose). -->
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <router-link
              :to="`/s/${s.id}`"
              class="truncate text-base font-semibold text-fg hover:text-accent"
              @click.stop
            >
              {{ s.branch.title || s.branch.name }}
            </router-link>
            <!-- Lifecycle: demoted, neutral, mono pill (StatusBadge). -->
            <StatusBadge :status="s.status" class="shrink-0" />
          </div>

          <!-- Current-state headline (agent's set-status message), else the goal. -->
          <p
            v-if="messageOf(s)"
            class="mt-0.5 truncate text-sm text-muted"
          >
            {{ messageOf(s) }}
          </p>
          <p
            v-if="s.branch.goal"
            class="mt-0.5 truncate text-xs text-faint"
          >
            {{ s.branch.goal }}
          </p>
        </div>

        <!-- Ref: machine identity, mono, pushed far-right and receding. -->
        <div class="shrink-0 text-right">
          <span class="block truncate font-mono text-xs text-faint">{{ s.branch.branch }}</span>
          <router-link
            v-if="s.branch.open_issue_count"
            :to="`/s/${s.id}?tab=issues`"
            class="font-mono text-xs text-muted hover:text-accent"
            @click.stop
          >
            {{ s.branch.open_issue_count }} open issue{{ s.branch.open_issue_count === 1 ? '' : 's' }}
          </router-link>
          <span v-if="s.last_activity_at" class="mt-0.5 block text-xs text-faint">
            {{ timeAgo(s.last_activity_at) }}
          </span>
        </div>
      </li>
    </ul>
  </div>
</template>
