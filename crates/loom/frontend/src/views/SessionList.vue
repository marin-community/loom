<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue';
import { get, post } from '../api';
import type { Session, RecentRepo, RepoBranch } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import AttentionBadge from '../components/AttentionBadge.vue';

const sessions = ref<Session[]>([]);

// Attention filter — the dashboard's "which sessions need me?" control.
type AttentionFilter = 'all' | 'attention' | 'ok';
const filter = ref<AttentionFilter>('all');

// Treat an unset/unknown level as 'ok' so older rows count as fine.
function levelOf(s: Session): string {
  return s.branch.attention || 'ok';
}

const counts = computed(() => {
  const c = { all: sessions.value.length, attention: 0, ok: 0 };
  for (const s of sessions.value) {
    if (levelOf(s) === 'ok') c.ok += 1;
    else c.attention += 1; // 'attention' and 'blocked' both "need me"
  }
  return c;
});

const filteredSessions = computed(() => {
  if (filter.value === 'all') return sessions.value;
  if (filter.value === 'ok') return sessions.value.filter((s) => levelOf(s) === 'ok');
  return sessions.value.filter((s) => levelOf(s) !== 'ok');
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
    await post('/sessions', body);
    title.value = '';
    goal.value = '';
    model.value = '';
    effort.value = '';
    name.value = '';
    existingBranch.value = '';
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
          <p v-if="branchesError" class="mt-1 text-xs text-red-400">{{ branchesError }}</p>
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
      <button
        type="submit"
        :disabled="creating"
        class="rounded bg-accent hover:bg-accent-hover px-3 py-1.5 text-sm font-medium disabled:opacity-50"
      >
        {{ creating ? 'Creating…' : 'Create' }}
      </button>
    </form>

    <p v-if="error" class="mb-4 text-sm text-red-400">{{ error }}</p>

    <p v-if="!sessions.length" class="text-muted text-sm">
      No sessions yet.
    </p>

    <!-- Attention filter: jump straight to the sessions that need a human. -->
    <div v-if="sessions.length" class="mb-3 inline-flex rounded border border-line text-xs overflow-hidden">
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

    <div v-if="sessions.length" class="overflow-x-auto rounded border border-line">
      <table class="w-full border-collapse text-sm">
        <thead>
          <tr class="border-b border-line bg-surface text-left text-xs uppercase tracking-wide text-muted">
            <th class="px-3 py-2 font-medium">Attention</th>
            <th class="px-3 py-2 font-medium">Title</th>
            <th class="px-3 py-2 font-medium">Goal</th>
            <th class="px-3 py-2 font-medium">Latest status</th>
            <th class="px-3 py-2 font-medium">Ref</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="s in filteredSessions"
            :key="s.id"
            data-testid="session-card"
            :data-session-id="s.id"
            class="border-b border-line last:border-0 hover:bg-surface cursor-pointer"
            @click="$router.push(`/s/${s.id}`)"
          >
            <td class="px-3 py-2 align-top">
              <router-link :to="`/s/${s.id}`" class="block space-y-1" @click.stop>
                <AttentionBadge :level="levelOf(s)" :note="s.branch.attention_note" />
                <span v-if="s.branch.attention_note" class="block max-w-[14rem] truncate text-xs text-muted">
                  {{ s.branch.attention_note }}
                </span>
                <StatusBadge :status="s.status" class="opacity-80" />
              </router-link>
            </td>
            <td class="px-3 py-2 align-top">
              <router-link
                :to="`/s/${s.id}`"
                class="block max-w-[16rem] truncate font-medium text-fg hover:text-accent"
                @click.stop
              >
                {{ s.branch.title || s.branch.name }}
              </router-link>
            </td>
            <td class="px-3 py-2 align-top">
              <span class="block max-w-[20rem] truncate text-muted">{{ s.branch.goal }}</span>
            </td>
            <td class="px-3 py-2 align-top">
              <span class="block max-w-[24rem] truncate text-muted">{{ s.branch.description }}</span>
            </td>
            <td class="px-3 py-2 align-top">
              <div class="font-mono text-xs text-faint">
                <span class="block truncate">{{ s.id }}</span>
                <span class="block truncate">{{ s.branch.branch }}</span>
                <span v-if="s.branch.open_issue_count" class="text-muted">
                  {{ s.branch.open_issue_count }} open issue{{ s.branch.open_issue_count === 1 ? '' : 's' }}
                </span>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
