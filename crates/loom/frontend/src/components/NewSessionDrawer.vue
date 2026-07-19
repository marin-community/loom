<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { get, listAgents, listRepos, post, registerRepo } from '../api';
import type { AgentMetadata, ManagedRepo, RecentRepo, RepoBranch } from '../types';
import AgentRuntimePicker from './AgentRuntimePicker.vue';
import ScratchPicker from './ScratchPicker.vue';

const emit = defineEmits<{
  close: [];
  created: [];
}>();

const recentRepos = ref<RecentRepo[]>([]);
const managedRepos = ref<ManagedRepo[]>([]);
const error = ref('');
const repo = ref('');
const repoFocused = ref(false);
const title = ref('');
const goal = ref('');
const agent = ref('');
const model = ref('');
const effort = ref('');
const name = ref('');
const nameEdited = ref(false);
const base = ref('');
const creating = ref(false);
const scratchFiles = ref<File[]>([]);
const agents = ref<AgentMetadata[]>([]);
const cloningRepo = ref(false);

// Show the platform's submit modifier (⌘ on macOS, Ctrl elsewhere). Both are
// wired up on the form; this is only the label.
const metaKeyLabel = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl';

const selectedAgent = computed<AgentMetadata | undefined>(() => {
  const selected = agent.value || agents.value[0]?.kind || '';
  return agents.value.find((a) => a.kind === selected);
});

type BranchMode = 'new' | 'existing';
const branchMode = ref<BranchMode>('new');
const existingBranch = ref('');
const branchFocused = ref(false);
const branches = ref<RepoBranch[]>([]);
const branchesError = ref('');
let branchesReqId = 0;

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 40);
}

function repoName(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() || path;
}

const REPO_SLUG = /^[A-Za-z0-9][\w.-]*\/[A-Za-z0-9][\w.-]*$/;

function looksLikeRemoteRepo(s: string): boolean {
  const q = s.trim();
  return REPO_SLUG.test(q) || /^https?:\/\//.test(q) || q.startsWith('git@');
}

const cloneCandidate = computed(() => {
  const q = repo.value.trim();
  if (!looksLikeRemoteRepo(q)) return '';
  const known = managedRepos.value.some((r) => r.slug === q || r.remote_url === q);
  return known ? '' : q;
});

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

async function loadManagedRepos() {
  try {
    managedRepos.value = await listRepos();
  } catch {
    // Managed-repo suggestions are a convenience; ignore failures here.
  }
}

async function addAndCloneRepo() {
  const q = cloneCandidate.value;
  if (!q) return;
  cloningRepo.value = true;
  try {
    const added = await registerRepo(q);
    await loadManagedRepos();
    repo.value = added.slug;
    repoFocused.value = false;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    cloningRepo.value = false;
  }
}

function pickBranch(b: RepoBranch) {
  existingBranch.value = b.name;
  branchFocused.value = false;
}

watch([title, goal], ([t, g]) => {
  if (!nameEdited.value) name.value = slugify(t || g);
});

watch(selectedAgent, (meta) => {
  if (!meta) return;
  if (model.value && !meta.accepts_raw_model && !meta.models.some((m) => m.id === model.value)) {
    model.value = '';
  }
  if (effort.value && !meta.efforts.some((e) => e.id === effort.value)) effort.value = '';
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

watch([repo, branchMode], ([, mode]) => {
  if (mode === 'existing') loadBranches();
});

async function loadRecentRepos() {
  try {
    recentRepos.value = (await get('/repos/recent')) as RecentRepo[];
  } catch {
    // The recent-repos dropdown is a convenience; ignore failures here.
  }
}

async function loadAgents() {
  try {
    const res = await listAgents();
    agents.value = res.agents;
    if (!agent.value) {
      agent.value = agents.value.some((a) => a.kind === res.default_agent)
        ? res.default_agent
        : agents.value[0]?.kind || '';
    }
  } catch (e) {
    error.value = (e as Error).message;
  }
}

function resetForm() {
  title.value = '';
  goal.value = '';
  agent.value = agents.value[0]?.kind || '';
  model.value = '';
  effort.value = '';
  name.value = '';
  base.value = '';
  existingBranch.value = '';
  scratchFiles.value = [];
  nameEdited.value = false;
  branchMode.value = 'new';
}

function cancel() {
  resetForm();
  emit('close');
}

async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

async function create() {
  if (!repo.value.trim() || !(title.value.trim() || goal.value.trim())) return;
  if (branchMode.value === 'existing' && !existingBranch.value.trim()) return;
  creating.value = true;
  try {
    const repoInput = repo.value.trim();
    const body: Record<string, unknown> = {
      title: title.value || undefined,
      goal: goal.value,
      agent: agent.value || undefined,
      model: model.value || undefined,
      effort: effort.value || undefined,
    };
    // A remote reference travels as `repo`: the server registers it if it is new
    // and clones it on the way, so an unknown `owner/name` needs no separate
    // "add the repo" step here. A path travels as `cwd`.
    if (looksLikeRemoteRepo(repoInput)) {
      body.repo = repoInput;
    } else {
      body.cwd = repoInput;
    }
    if (branchMode.value === 'existing') {
      body.existing_branch = existingBranch.value.trim();
    } else {
      body.name = name.value || undefined;
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
    emit('created');
    emit('close');
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

loadRecentRepos();
loadManagedRepos();
loadAgents();
</script>

<template>
  <!--
    Autofill suppression: Chrome's address/payment classifier deliberately
    ignores autocomplete="off". Unrecognized per-field tokens keep this workflow
    out of contact/payment autofill while preserving the form's normal keyboard
    behavior.
  -->
  <form
    class="mb-4 flex max-h-[calc(100vh-7rem)] max-w-4xl flex-col rounded-md border border-line bg-surface"
    autocomplete="off"
    data-testid="new-session-drawer"
    @submit.prevent="create"
    @keydown.enter.meta.prevent="create"
    @keydown.enter.ctrl.prevent="create"
  >
    <div class="border-b border-line px-4 py-3">
      <h2 class="text-sm font-semibold text-fg">New session</h2>
      <p class="mt-0.5 text-xs text-faint">Choose the repo, task, runtime, and branch shape.</p>
    </div>

    <div class="grid min-h-0 gap-5 overflow-auto p-4 lg:grid-cols-[minmax(0,1fr)_18rem]">
      <div class="space-y-5">
        <section class="space-y-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Repository</h3>
          <div class="relative">
            <label class="block text-xs text-muted mb-1">
              Repository - a server path, or a GitHub <span class="font-mono">owner/name</span> to
              clone
              <span v-if="recentRepos.length" class="text-faint">- or pick a recent one</span>
            </label>
            <input
              v-model="repo"
              @focus="repoFocused = true"
              @input="repoFocused = true"
              @blur="repoFocused = false"
              placeholder="owner/name or /home/you/code/project"
              autocomplete="loom-repo"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            />
            <ul
              v-if="repoFocused && (repoMatches.length || cloneCandidate)"
              data-testid="recent-repos"
              class="absolute left-0 right-0 z-20 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
            >
              <li v-if="cloneCandidate">
                <button
                  type="button"
                  data-testid="clone-repo"
                  :disabled="cloningRepo"
                  class="flex w-full items-center gap-2 px-2 py-1.5 text-left text-accent hover:bg-subtle disabled:opacity-60"
                  @mousedown.prevent="addAndCloneRepo"
                >
                  <span class="shrink-0 text-sm">+ Clone new repo</span>
                  <span class="min-w-0 truncate font-mono text-xs text-muted">{{
                    cloneCandidate
                  }}</span>
                  <span v-if="cloningRepo" class="ml-auto shrink-0 text-2xs text-faint"
                    >adding...</span
                  >
                </button>
              </li>
              <li v-for="r in repoMatches" :key="r.repo_root">
                <button
                  type="button"
                  data-testid="recent-repo"
                  @mousedown.prevent="pickRepo(r.repo_root)"
                  class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-subtle"
                >
                  <span class="min-w-0">
                    <span class="block truncate text-sm">{{ repoName(r.repo_root) }}</span>
                    <span class="block truncate text-xs text-muted font-mono">{{
                      r.repo_root
                    }}</span>
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

        <section class="space-y-3 border-t border-line pt-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">What to build</h3>
          <div>
            <label class="block text-xs text-muted mb-1">Title</label>
            <input
              v-model="title"
              placeholder="Health endpoint"
              autocomplete="loom-title"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            />
          </div>
          <div>
            <label class="block text-xs text-muted mb-1">
              Goal - optional; leave blank to start the agent with no prompt
            </label>
            <textarea
              v-model="goal"
              rows="4"
              placeholder="Add a /health endpoint that returns 200"
              autocomplete="loom-goal"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
            ></textarea>
          </div>
        </section>

        <section class="space-y-3 border-t border-line pt-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Branch</h3>
          <div>
            <div class="inline-flex rounded border border-line text-xs overflow-hidden mb-2">
              <button
                type="button"
                :class="[
                  'px-3 py-1',
                  branchMode === 'new'
                    ? 'bg-accent text-accent-fg'
                    : 'bg-input text-muted hover:bg-subtle',
                ]"
                @click="branchMode = 'new'"
              >
                New branch
              </button>
              <button
                type="button"
                :class="[
                  'px-3 py-1 border-l border-line',
                  branchMode === 'existing'
                    ? 'bg-accent text-accent-fg'
                    : 'bg-input text-muted hover:bg-subtle',
                ]"
                @click="branchMode = 'existing'"
              >
                Existing branch
              </button>
            </div>
            <div v-if="branchMode === 'new'" class="space-y-2">
              <div>
                <label class="block text-xs text-muted mb-1">
                  Name - the worktree (<code>.worktrees/&lt;name&gt;</code>) and branch
                  (<code>weaver/&lt;name&gt;</code>)
                </label>
                <input
                  v-model="name"
                  @input="nameEdited = true"
                  placeholder="health-endpoint"
                  autocomplete="loom-branch-name"
                  spellcheck="false"
                  class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
                />
              </div>
              <div>
                <label class="block text-xs text-muted mb-1">
                  Base branch - fork point (optional)
                </label>
                <input
                  v-model="base"
                  placeholder="origin/main (freshly fetched)"
                  autocomplete="loom-base-branch"
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
                Existing branch - weaver reuses its worktree if one is checked out
              </label>
              <input
                v-model="existingBranch"
                @focus="branchFocused = true"
                @input="branchFocused = true"
                @blur="branchFocused = false"
                placeholder="feature/foo"
                autocomplete="loom-existing-branch"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
              />
              <p v-if="branchesError" class="mt-1 text-xs text-block">{{ branchesError }}</p>
              <ul
                v-if="branchFocused && branchMatches.length"
                data-testid="branch-options"
                class="absolute left-0 right-0 z-20 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
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
                      <span v-if="b.worktree" class="block truncate text-xs text-muted font-mono"
                        >-&gt; {{ b.worktree }}</span
                      >
                    </span>
                  </button>
                </li>
              </ul>
            </div>
          </div>
        </section>

        <section class="space-y-3 border-t border-line pt-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Scratch files</h3>
          <ScratchPicker v-model="scratchFiles" />
        </section>
      </div>

      <aside class="space-y-4 border-t border-line pt-4 lg:border-l lg:border-t-0 lg:pl-4 lg:pt-0">
        <section class="space-y-3">
          <div>
            <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">
              Runtime profile
            </h3>
            <p class="mt-1 text-xs text-faint">
              Agent runtime, model selector, and reasoning effort.
            </p>
          </div>

          <AgentRuntimePicker
            :agents="agents"
            :agent-kind="agent"
            :model="model"
            :effort="effort"
            agent-grid-class="grid gap-2"
            choice-grid-class="grid gap-4"
            :show-agent-badges="false"
            show-agent-counts
            raw-model-id="session-model"
            raw-model-autocomplete="loom-model"
            @update:agent="agent = $event"
            @update:model="model = $event"
            @update:effort="effort = $event"
          />
        </section>
      </aside>
    </div>

    <p v-if="error" class="border-t border-line px-4 py-2 text-sm text-block">{{ error }}</p>

    <div class="flex items-center gap-2 border-t border-line px-4 py-3">
      <button
        type="submit"
        :disabled="creating"
        class="btn-primary px-3 py-1.5 text-sm font-medium"
      >
        {{ creating ? 'Creating...' : 'Create' }}
      </button>
      <button type="button" class="btn-secondary px-3 py-1.5 text-sm font-medium" @click="cancel">
        Cancel
      </button>
      <!-- Keyboard affordance: submit from anywhere in the form (the goal
           textarea swallows a plain Enter) without reaching for the mouse. -->
      <span class="ml-auto text-2xs text-faint">
        <kbd class="font-mono">{{ metaKeyLabel }}</kbd> + <kbd class="font-mono">Enter</kbd> to
        create
      </span>
    </div>
  </form>
</template>
