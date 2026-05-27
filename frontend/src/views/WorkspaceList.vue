<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue';
import { get, post } from '../api';
import type { Workspace, RecentRepo, Branch } from '../types';
import StatusBadge from '../components/StatusBadge.vue';

const workspaces = ref<Workspace[]>([]);
const recentRepos = ref<RecentRepo[]>([]);
const error = ref('');
const showForm = ref(false);
const repo = ref('');
const repoFocused = ref(false);
const title = ref('');
const goal = ref('');
const name = ref('');
const nameEdited = ref(false);
const creating = ref(false);

type BranchMode = 'new' | 'existing';
const branchMode = ref<BranchMode>('new');
const existingBranch = ref('');
const branchFocused = ref(false);
const branches = ref<Branch[]>([]);
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

function pickBranch(b: Branch) {
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
    const res = (await get(`/repos/branches?cwd=${encodeURIComponent(path)}`)) as Branch[];
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
    workspaces.value = (await get('/workspaces')) as Workspace[];
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
  // A workspace needs a repo and at least a title or a goal; the goal alone
  // is optional (an empty goal just starts the agent unprompted).
  if (!repo.value.trim() || !(title.value.trim() || goal.value.trim())) return;
  if (branchMode.value === 'existing' && !existingBranch.value.trim()) return;
  creating.value = true;
  try {
    const body: Record<string, unknown> = {
      cwd: repo.value,
      title: title.value || undefined,
      goal: goal.value,
    };
    if (branchMode.value === 'existing') {
      body.existing_branch = existingBranch.value.trim();
    } else {
      body.name = name.value || undefined;
    }
    await post('/workspaces', body);
    title.value = '';
    goal.value = '';
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
      <h1 class="text-xl font-semibold">Workspaces</h1>
      <button
        class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm font-medium"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New workspace' }}
      </button>
    </div>

    <form
      v-if="showForm"
      class="mb-5 rounded border border-neutral-800 bg-neutral-900 p-4 space-y-3"
      @submit.prevent="create"
    >
      <div class="relative">
        <label class="block text-xs text-neutral-400 mb-1">
          Repository path (on the server)
          <span v-if="recentRepos.length" class="text-neutral-600">— or pick a recent one</span>
        </label>
        <input
          v-model="repo"
          @focus="repoFocused = true"
          @input="repoFocused = true"
          @blur="repoFocused = false"
          placeholder="/home/you/code/project"
          autocomplete="off"
          spellcheck="false"
          class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
        />
        <ul
          v-if="repoFocused && repoMatches.length"
          data-testid="recent-repos"
          class="absolute left-0 right-0 z-10 mt-1 max-h-56 overflow-auto rounded border border-neutral-700 bg-neutral-800 shadow-lg"
        >
          <li v-for="r in repoMatches" :key="r.repo_root">
            <button
              type="button"
              data-testid="recent-repo"
              @mousedown.prevent="pickRepo(r.repo_root)"
              class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-neutral-700"
            >
              <span class="min-w-0">
                <span class="block truncate text-sm">{{ repoName(r.repo_root) }}</span>
                <span class="block truncate text-xs text-neutral-500 font-mono">{{ r.repo_root }}</span>
              </span>
              <span
                v-if="r.active_workspaces"
                :title="`${r.active_workspaces} active workspace(s)`"
                class="shrink-0 rounded bg-neutral-700 px-1.5 py-0.5 text-xs text-neutral-300"
              >
                {{ r.active_workspaces }}
              </span>
            </button>
          </li>
        </ul>
      </div>
      <div>
        <label class="block text-xs text-neutral-400 mb-1">Title</label>
        <input
          v-model="title"
          placeholder="Health endpoint"
          class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
        />
      </div>
      <div>
        <label class="block text-xs text-neutral-400 mb-1">
          Goal — optional; leave blank to start the agent with no prompt
        </label>
        <input
          v-model="goal"
          placeholder="Add a /health endpoint that returns 200"
          class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
        />
      </div>
      <div>
        <div class="inline-flex rounded border border-neutral-700 text-xs overflow-hidden mb-2">
          <button
            type="button"
            :class="[
              'px-3 py-1',
              branchMode === 'new' ? 'bg-emerald-700 text-white' : 'bg-neutral-800 text-neutral-300 hover:bg-neutral-700',
            ]"
            @click="branchMode = 'new'"
          >
            New branch
          </button>
          <button
            type="button"
            :class="[
              'px-3 py-1 border-l border-neutral-700',
              branchMode === 'existing' ? 'bg-emerald-700 text-white' : 'bg-neutral-800 text-neutral-300 hover:bg-neutral-700',
            ]"
            @click="branchMode = 'existing'"
          >
            Existing branch
          </button>
        </div>
        <div v-if="branchMode === 'new'">
          <label class="block text-xs text-neutral-400 mb-1">
            Name — the worktree (<code>.worktrees/&lt;name&gt;</code>) and branch
            (<code>weaver/&lt;name&gt;</code>)
          </label>
          <input
            v-model="name"
            @input="nameEdited = true"
            placeholder="health-endpoint"
            class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600 font-mono"
          />
        </div>
        <div v-else class="relative">
          <label class="block text-xs text-neutral-400 mb-1">
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
            class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600 font-mono"
          />
          <p v-if="branchesError" class="mt-1 text-xs text-red-400">{{ branchesError }}</p>
          <ul
            v-if="branchFocused && branchMatches.length"
            data-testid="branch-options"
            class="absolute left-0 right-0 z-10 mt-1 max-h-56 overflow-auto rounded border border-neutral-700 bg-neutral-800 shadow-lg"
          >
            <li v-for="b in branchMatches" :key="b.name">
              <button
                type="button"
                data-testid="branch-option"
                @mousedown.prevent="pickBranch(b)"
                class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-neutral-700"
              >
                <span class="min-w-0">
                  <span class="block truncate text-sm font-mono">
                    {{ b.name }}
                    <span v-if="b.current" class="ml-1 text-xs text-emerald-400">(current)</span>
                  </span>
                  <span
                    v-if="b.worktree"
                    class="block truncate text-xs text-neutral-500 font-mono"
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
        class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm font-medium disabled:opacity-50"
      >
        {{ creating ? 'Creating…' : 'Create' }}
      </button>
    </form>

    <p v-if="error" class="mb-4 text-sm text-red-400">{{ error }}</p>

    <p v-if="!workspaces.length" class="text-neutral-500 text-sm">
      No workspaces yet.
    </p>

    <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <router-link
        v-for="w in workspaces"
        :key="w.id"
        :to="`/w/${w.id}`"
        data-testid="workspace-card"
        :data-workspace-id="w.id"
        class="block rounded border border-neutral-800 bg-neutral-900 p-4 hover:border-neutral-600 transition"
      >
        <div class="flex items-center justify-between gap-2 mb-2">
          <span class="font-medium truncate">{{ w.title || w.name }}</span>
          <StatusBadge :status="w.status" />
        </div>
        <p v-if="w.goal" class="text-sm text-neutral-300 line-clamp-2 mb-2">{{ w.goal }}</p>
        <p v-if="w.description" class="text-xs text-neutral-500 line-clamp-3 mb-2">
          {{ w.description }}
        </p>
        <div class="text-xs text-neutral-600 font-mono">{{ w.id }} · {{ w.branch }}</div>
      </router-link>
    </div>
  </div>
</template>
