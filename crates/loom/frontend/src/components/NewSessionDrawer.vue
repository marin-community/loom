<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import {
  ApiError,
  cloneProfile,
  get,
  listAgents,
  listProfiles,
  listRepos,
  post,
  registerRepo,
  resolveSessionLaunch,
} from '../api';
import type {
  AgentMetadata,
  LaunchOverrides as LaunchOverrideValues,
  LaunchSelection,
  ManagedRepo,
  Profile,
  RecentRepo,
  RepoBranch,
  ResolvedLaunch,
  Session,
} from '../types';
import LaunchOverrides from './LaunchOverrides.vue';
import ProfileSelector from './ProfileSelector.vue';
import ResolvedLaunchSummary from './ResolvedLaunchSummary.vue';
import ScratchPicker from './ScratchPicker.vue';

const emit = defineEmits<{
  close: [];
  created: [];
}>();
const router = useRouter();

const recentRepos = ref<RecentRepo[]>([]);
const managedRepos = ref<ManagedRepo[]>([]);
const error = ref('');
const repo = ref('');
const repoFocused = ref(false);
const title = ref('');
const goal = ref('');
const name = ref('');
const nameEdited = ref(false);
const base = ref('');
const creating = ref(false);
const scratchFiles = ref<File[]>([]);
const agents = ref<AgentMetadata[]>([]);
const profiles = ref<Profile[]>([]);
const profile = ref('default');
const overrides = ref<LaunchOverrideValues>({});
const resolved = ref<ResolvedLaunch | null>(null);
const resolving = ref(false);
const resolveError = ref('');
const advanced = ref(false);
const cloneName = ref('');
const copyEnvironment = ref(false);
const cloneBusy = ref(false);
const cloneNotice = ref('');
const cloningRepo = ref(false);

// Show the platform's submit modifier (⌘ on macOS, Ctrl elsewhere). Both are
// wired up on the form; this is only the label.
const metaKeyLabel = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl';

const selection = computed<LaunchSelection>(() => ({
  profile: profile.value || 'default',
  overrides: { ...overrides.value },
}));

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
    const [metadata, templates] = await Promise.all([listAgents(), listProfiles()]);
    agents.value = metadata.agents;
    profiles.value = templates;
    if (!templates.some((item) => item.name === profile.value)) {
      profile.value = templates[0]?.name ?? 'default';
    }
  } catch (e) {
    error.value = (e as Error).message;
  }
}

function resetForm() {
  title.value = '';
  goal.value = '';
  profile.value = profiles.value.some((item) => item.name === 'default')
    ? 'default'
    : (profiles.value[0]?.name ?? 'default');
  overrides.value = {};
  name.value = '';
  base.value = '';
  existingBranch.value = '';
  scratchFiles.value = [];
  nameEdited.value = false;
  branchMode.value = 'new';
}

function cancel() {
  emit('close');
  void router.push('/');
}

let resolveRequest = 0;
let resolveTimer: ReturnType<typeof setTimeout> | undefined;
watch(
  selection,
  () => {
    if (resolveTimer) clearTimeout(resolveTimer);
    resolveTimer = setTimeout(() => void resolveSelection(), 120);
  },
  { deep: true },
);

async function resolveSelection() {
  if (!profiles.value.length) return;
  const request = ++resolveRequest;
  resolving.value = true;
  resolveError.value = '';
  try {
    const preview = await resolveSessionLaunch(selection.value);
    if (request === resolveRequest) resolved.value = preview;
  } catch (cause) {
    if (request !== resolveRequest) return;
    resolved.value = null;
    resolveError.value = (cause as Error).message;
  } finally {
    if (request === resolveRequest) resolving.value = false;
  }
}

function chooseProfile(value: string) {
  profile.value = value;
  overrides.value = {};
  cloneNotice.value = '';
}

async function saveAsNewProfile() {
  const preview = resolved.value;
  const target = cloneName.value.trim();
  if (!preview || !target) return;
  cloneBusy.value = true;
  cloneNotice.value = '';
  error.value = '';
  try {
    const saved = await cloneProfile(preview.selection.profile, {
      name: target,
      expected_profile_revision: preview.profile_revision,
      overrides: { ...overrides.value },
      copy_environment: copyEnvironment.value,
    });
    profiles.value = await listProfiles();
    chooseProfile(saved.name);
    cloneName.value = '';
    copyEnvironment.value = false;
    cloneNotice.value = `Saved ${saved.name} without changing ${preview.selection.profile}.`;
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    cloneBusy.value = false;
  }
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
  if (creating.value || resolving.value) return;
  if (!repo.value.trim() || !(title.value.trim() || goal.value.trim())) return;
  if (branchMode.value === 'existing' && !existingBranch.value.trim()) return;
  if (!resolved.value?.valid) return;
  creating.value = true;
  try {
    const repoInput = repo.value.trim();
    const body: Record<string, unknown> = {
      title: title.value || undefined,
      goal: goal.value,
      selection: selection.value,
      expected_profile_revision: resolved.value?.profile_revision,
      expected_resolver_revision: resolved.value?.resolver_revision,
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
    const session = (await post('/sessions', body)) as Session;
    resetForm();
    emit('created');
    emit('close');
    await router.push(`/s/${session.id}`);
  } catch (e) {
    const preview = e instanceof ApiError ? e.body.preview : undefined;
    if (preview && typeof preview === 'object') {
      resolved.value = preview as ResolvedLaunch;
    }
    const failedSession = e instanceof ApiError ? e.body.session_id : undefined;
    if (typeof failedSession === 'string' && failedSession) {
      // Provisioning succeeded far enough to leave a recoverable session, but
      // its agent setup failed. Refresh the fleet and take the user straight to
      // the visible error/handoff controls instead of marooning the drawer.
      emit('created');
      resetForm();
      emit('close');
      await router.push(`/s/${failedSession}`);
      return;
    }
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

function onFormKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || (!event.metaKey && !event.ctrlKey)) return;
  event.preventDefault();
  void create();
}

loadRecentRepos();
loadManagedRepos();
void loadAgents().then(resolveSelection);
</script>

<template>
  <!--
    Autofill suppression: Chrome's address/payment classifier deliberately
    ignores autocomplete="off". Unrecognized per-field tokens keep this workflow
    out of contact/payment autofill while preserving the form's normal keyboard
    behavior.
  -->
  <form
    class="mx-auto flex min-h-full w-full max-w-7xl flex-1 flex-col bg-canvas"
    autocomplete="off"
    data-testid="new-session-drawer"
    @submit.prevent="create"
    @keydown="onFormKeydown"
  >
    <div class="border-b border-line bg-surface px-5 py-4 sm:px-8">
      <p class="text-2xs font-semibold uppercase tracking-wider text-muted">Sessions / New</p>
      <h1 class="mt-1 font-serif text-2xl font-semibold text-fg">Launch a session</h1>
      <p class="mt-1 text-sm text-muted">
        Pick a reusable profile, make one-launch changes, and review the server’s exact snapshot.
      </p>
    </div>

    <div
      class="grid min-h-0 flex-1 gap-6 overflow-auto p-5 sm:p-8 lg:grid-cols-[minmax(0,1fr)_25rem]"
    >
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

      <aside class="space-y-4 border-t border-line pt-4 lg:border-l lg:border-t-0 lg:pl-6 lg:pt-0">
        <section class="space-y-3">
          <div>
            <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">
              Launch profile
            </h3>
            <p class="mt-1 text-xs text-faint">
              Templates stay unchanged unless you explicitly save a new one.
            </p>
          </div>

          <ProfileSelector
            :profiles="profiles"
            :model-value="profile"
            layout="list"
            :disabled="creating"
            @update:model-value="chooseProfile"
          />
          <RouterLink to="/settings" class="inline-flex text-xs text-accent hover:underline">
            Edit profile templates in Settings
          </RouterLink>
        </section>

        <section class="space-y-2 rounded-md border border-line bg-surface p-3">
          <button
            type="button"
            class="flex w-full items-center justify-between text-left text-xs font-medium text-fg"
            :aria-expanded="advanced"
            @click="advanced = !advanced"
          >
            One-launch overrides
            <span class="text-faint">{{ advanced ? 'Hide' : 'Edit' }}</span>
          </button>
          <p v-if="resolved?.policy.strict" class="text-xs text-faint">
            This strict profile locks every launch selector.
          </p>
          <LaunchOverrides
            v-if="advanced"
            v-model="overrides"
            :agents="agents"
            :resolved="resolved"
            :disabled="Boolean(resolved?.policy.strict)"
          />
        </section>

        <p v-if="resolveError" class="rounded bg-block-soft p-2 text-xs text-block">
          {{ resolveError }}
        </p>
        <ResolvedLaunchSummary :resolved="resolved" :loading="resolving" />

        <section v-if="resolved" class="space-y-2 rounded-md border border-line bg-surface p-3">
          <h3 class="text-xs font-medium text-fg">Save these settings as a new profile</h3>
          <p class="text-xs text-faint">
            The server clones policy from <code>{{ resolved.selection.profile }}</code
            >; the source template is never overwritten.
          </p>
          <div class="flex gap-2">
            <input
              v-model="cloneName"
              data-testid="clone-profile-name"
              placeholder="profile-name"
              class="min-w-0 flex-1 rounded bg-input px-2 py-1.5 font-mono text-xs"
            />
            <button
              type="button"
              data-testid="clone-profile"
              class="btn-secondary px-2.5 py-1.5 text-xs"
              :disabled="cloneBusy || !cloneName.trim()"
              @click="saveAsNewProfile"
            >
              {{ cloneBusy ? 'Saving…' : 'Save new' }}
            </button>
          </div>
          <label class="flex items-center gap-2 text-xs text-muted">
            <input v-model="copyEnvironment" type="checkbox" />
            Copy write-only environment values
          </label>
          <p v-if="cloneNotice" class="text-xs text-ok">{{ cloneNotice }}</p>
        </section>
      </aside>
    </div>

    <p v-if="error" class="border-t border-line bg-surface px-5 py-2 text-sm text-block sm:px-8">
      {{ error }}
    </p>

    <div
      class="sticky bottom-0 flex items-center gap-2 border-t border-line bg-surface px-5 py-3 sm:px-8"
    >
      <button
        type="submit"
        data-testid="create-session"
        :disabled="creating || resolving || !resolved?.valid"
        class="btn-primary px-3 py-1.5 text-sm font-medium"
      >
        {{ creating ? 'Creating...' : 'Create session' }}
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
