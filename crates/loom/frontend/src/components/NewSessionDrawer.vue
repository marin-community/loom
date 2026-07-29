<script setup lang="ts">
import { computed, nextTick, onActivated, ref, watch } from 'vue';
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
const repoActiveOption = ref(-1);
const title = ref('');
const goal = ref('');
const name = ref('');
const nameEdited = ref(false);
const base = ref('');
const creating = ref(false);
const scratchFiles = ref<File[]>([]);
const scratchError = ref('');
const scratchPicker = ref<InstanceType<typeof ScratchPicker> | null>(null);
const errorElement = ref<HTMLElement | null>(null);
const agents = ref<AgentMetadata[]>([]);
const profiles = ref<Profile[]>([]);
const profile = ref('default');
const overrides = ref<LaunchOverrideValues>({});
const resolved = ref<ResolvedLaunch | null>(null);
const lastResolved = ref<ResolvedLaunch | null>(null);
const resolving = ref(false);
const resolveError = ref('');
const cloneName = ref('');
const clonePreviewKey = ref('');
const cloneBusy = ref(false);
const cloneNotice = ref('');
const repoRegistrations = ref(0);
const cloningRepo = computed(() => repoRegistrations.value > 0);
const repoError = ref('');
let repoRegistrationReq = 0;
let repoRegistrationTail: Promise<void> = Promise.resolve();
const registeredRepo = ref<ManagedRepo | null>(null);

// Show the platform's submit modifier (⌘ on macOS, Ctrl elsewhere). Both are
// wired up on the form; this is only the label.
const metaKeyLabel = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl';

const selection = computed<LaunchSelection>(() => ({
  profile: profile.value || 'default',
  overrides: { ...overrides.value },
}));
const selectedProfile = computed(() =>
  profiles.value.find((candidate) => candidate.name === profile.value),
);

type BranchMode = 'new' | 'existing';
const branchMode = ref<BranchMode>('new');
const existingBranch = ref('');
const branchFocused = ref(false);
const branchActiveOption = ref(-1);
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
  repoActiveOption.value = -1;
}

function nextOption(current: number, count: number, delta: -1 | 1): number {
  if (current < 0) return delta > 0 ? 0 : count - 1;
  return (current + delta + count) % count;
}

const REPOSITORY_OPTION_PREFIX = 'launch-repository-option';
const BRANCH_OPTION_PREFIX = 'launch-branch-option';

function optionId(prefix: string, index: number): string {
  return `${prefix}-${index}`;
}

function revealOption(prefix: string, index: number) {
  void nextTick(() => {
    document.getElementById(optionId(prefix, index))?.scrollIntoView({ block: 'nearest' });
  });
}

function onRepoInput() {
  repoFocused.value = true;
  repoActiveOption.value = -1;
}

function onRepoKeydown(event: KeyboardEvent) {
  const count = repoMatches.value.length + (cloneCandidate.value ? 1 : 0);
  if (event.key === 'Escape') {
    event.stopPropagation();
    repoFocused.value = false;
    repoActiveOption.value = -1;
    return;
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    if (!count) return;
    event.preventDefault();
    event.stopPropagation();
    repoFocused.value = true;
    const delta = event.key === 'ArrowDown' ? 1 : -1;
    repoActiveOption.value = nextOption(repoActiveOption.value, count, delta);
    revealOption(REPOSITORY_OPTION_PREFIX, repoActiveOption.value);
    return;
  }
  if (event.key !== 'Enter' || repoActiveOption.value < 0) return;
  event.preventDefault();
  event.stopPropagation();
  if (cloneCandidate.value && repoActiveOption.value === 0) void addAndCloneRepo();
  else {
    const index = repoActiveOption.value - (cloneCandidate.value ? 1 : 0);
    const match = repoMatches.value[index];
    if (match) pickRepo(match.repo_root);
  }
}

function rememberManagedRepo(added: ManagedRepo) {
  const next = managedRepos.value.filter(
    (candidate) => candidate.slug !== added.slug && candidate.remote_url !== added.remote_url,
  );
  managedRepos.value = [...next, added];
  registeredRepo.value = added;
}

async function registerDraftRepo(candidate: string): Promise<ManagedRepo> {
  repoRegistrations.value += 1;
  let release!: () => void;
  const predecessor = repoRegistrationTail;
  repoRegistrationTail = new Promise<void>((resolve) => {
    release = resolve;
  });
  try {
    await predecessor;
    const added = await registerRepo(candidate);
    rememberManagedRepo(added);
    return added;
  } finally {
    release();
    repoRegistrations.value -= 1;
  }
}

async function addAndCloneRepo() {
  const q = cloneCandidate.value;
  if (!q) return;
  const request = ++repoRegistrationReq;
  repoError.value = '';
  try {
    const added = await registerDraftRepo(q);
    if (request !== repoRegistrationReq || repo.value.trim() !== q) return;
    repo.value = added.slug;
    repoFocused.value = false;
  } catch (e) {
    if (request === repoRegistrationReq && repo.value.trim() === q) {
      repoError.value = (e as Error).message;
    }
  }
}

watch(repo, () => {
  ++repoRegistrationReq;
  const current = repo.value.trim();
  if (
    registeredRepo.value &&
    current !== registeredRepo.value.slug &&
    current !== registeredRepo.value.remote_url
  ) {
    registeredRepo.value = null;
  }
  repoError.value = '';
});

function pickBranch(b: RepoBranch) {
  existingBranch.value = b.name;
  branchFocused.value = false;
  branchActiveOption.value = -1;
}

function onBranchInput() {
  branchFocused.value = true;
  branchActiveOption.value = -1;
}

function onBranchKeydown(event: KeyboardEvent) {
  const count = branchMatches.value.length;
  if (event.key === 'Escape') {
    event.stopPropagation();
    branchFocused.value = false;
    branchActiveOption.value = -1;
    return;
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    if (!count) return;
    event.preventDefault();
    event.stopPropagation();
    branchFocused.value = true;
    const delta = event.key === 'ArrowDown' ? 1 : -1;
    branchActiveOption.value = nextOption(branchActiveOption.value, count, delta);
    revealOption(BRANCH_OPTION_PREFIX, branchActiveOption.value);
    return;
  }
  if (event.key === 'Enter' && branchActiveOption.value >= 0) {
    event.preventDefault();
    event.stopPropagation();
    const match = branchMatches.value[branchActiveOption.value];
    if (match) pickBranch(match);
  }
}

watch([title, goal], ([t, g]) => {
  if (!nameEdited.value) name.value = slugify(t || g);
});

async function loadBranches() {
  const reqId = ++branchesReqId;
  const path = repo.value.trim();
  branches.value = [];
  branchesError.value = '';
  if (!path) return;
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

function resetForm() {
  repo.value = '';
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
  cloneName.value = '';
  clonePreviewKey.value = '';
  cloneNotice.value = '';
  error.value = '';
  resolveError.value = '';
  branchesError.value = '';
  repoError.value = '';
  scratchError.value = '';
  ++resolveRequest;
  resolved.value = null;
  lastResolved.value = null;
  resolving.value = false;
  scratchPicker.value?.resetTransient();
}

function cancel() {
  if (creating.value) return;
  resetForm();
  emit('close');
  void router.replace('/');
}

let resolveRequest = 0;
let resolveTimer: ReturnType<typeof setTimeout> | undefined;

function scheduleResolution() {
  if (resolveTimer) clearTimeout(resolveTimer);
  const request = ++resolveRequest;
  resolved.value = null;
  cloneName.value = '';
  clonePreviewKey.value = '';
  resolving.value = true;
  resolveError.value = '';
  resolveTimer = setTimeout(() => {
    resolveTimer = undefined;
    void resolveSelection(request);
  }, 120);
}

watch(selection, scheduleResolution, { deep: true, flush: 'sync' });

async function resolveSelection(request: number) {
  if (!profiles.value.length) {
    if (request === resolveRequest) resolving.value = false;
    return;
  }
  try {
    const preview = await resolveSessionLaunch(selection.value);
    if (request === resolveRequest) {
      resolved.value = preview;
      lastResolved.value = preview;
    }
  } catch (cause) {
    if (request !== resolveRequest) return;
    resolved.value = null;
    resolveError.value = (cause as Error).message;
  } finally {
    if (request === resolveRequest) resolving.value = false;
  }
}

let activationRequest = 0;
async function refreshLaunchData() {
  const activation = ++activationRequest;
  if (resolveTimer) {
    clearTimeout(resolveTimer);
    resolveTimer = undefined;
  }
  ++resolveRequest;
  resolved.value = null;
  resolving.value = false;
  try {
    const [recent, managed, metadata, templates] = await Promise.all([
      get('/repos/recent').catch(() => recentRepos.value) as Promise<RecentRepo[]>,
      listRepos().catch(() => managedRepos.value),
      listAgents(),
      listProfiles(),
    ]);
    if (activation !== activationRequest) return;
    recentRepos.value = recent;
    managedRepos.value = managed;
    agents.value = metadata.agents;
    profiles.value = templates;
    if (!templates.some((item) => item.name === profile.value)) {
      profile.value = templates.some((item) => item.name === 'default')
        ? 'default'
        : (templates[0]?.name ?? 'default');
      lastResolved.value = null;
    }
  } catch (cause) {
    if (activation !== activationRequest) return;
    error.value = (cause as Error).message;
  }
  if (activation !== activationRequest) return;
  scheduleResolution();
}

function chooseProfile(value: string) {
  profile.value = value;
  overrides.value = {};
  lastResolved.value = null;
  cloneNotice.value = '';
}

const hasLaunchChanges = computed(() => Object.keys(overrides.value).length > 0);

function resetLaunchChanges() {
  overrides.value = {};
  cloneNotice.value = '';
}

const cloneStaticValid = computed(
  () =>
    Boolean(resolved.value) &&
    (resolved.value?.errors ?? []).every((message) => message.includes('max_concurrent')),
);

function previewKey(preview: ResolvedLaunch): string {
  return JSON.stringify({
    selection: preview.selection,
    profile_lifetime: preview.profile_lifetime,
    profile_revision: preview.profile_revision,
    resolver_revision: preview.resolver_revision,
  });
}

async function beginSaveAsNew() {
  const preview = resolved.value;
  if (!preview || !cloneStaticValid.value) return;
  cloneBusy.value = true;
  error.value = '';
  let source: Profile | undefined;
  try {
    const refreshed = await listProfiles();
    if (
      !resolved.value ||
      preview !== resolved.value ||
      previewKey(preview) !== previewKey(resolved.value)
    )
      return;
    profiles.value = refreshed;
    source = refreshed.find((item) => item.name === preview.selection.profile);
    if (
      !source ||
      source.revision !== preview.profile_revision ||
      source.lifetime !== preview.profile_lifetime
    ) {
      error.value =
        'The source profile changed after this preview. Review the fresh resolution before saving.';
      scheduleResolution();
      return;
    }
  } catch (cause) {
    error.value = (cause as Error).message;
    return;
  } finally {
    cloneBusy.value = false;
  }
  if (!source) return;
  cloneName.value = `${source.name}-copy`;
  clonePreviewKey.value = previewKey(preview);
  cloneNotice.value = '';
}

async function saveAsNewProfile() {
  const preview = resolved.value;
  const target = cloneName.value.trim();
  if (!preview || !target || clonePreviewKey.value !== previewKey(preview)) {
    cloneName.value = '';
    clonePreviewKey.value = '';
    error.value = 'Launch settings changed. Review the fresh resolution before saving a profile.';
    return;
  }
  cloneBusy.value = true;
  cloneNotice.value = '';
  error.value = '';
  const submittedKey = previewKey(preview);
  const submittedGeneration = resolveRequest;
  let saved: Profile;
  try {
    saved = await cloneProfile(preview.selection.profile, {
      name: target,
      expected_profile_revision: preview.profile_revision,
      expected_resolver_revision: preview.resolver_revision,
      overrides: { ...overrides.value },
      copy_environment: true,
    });
  } catch (cause) {
    const preview = cause instanceof ApiError ? cause.body.preview : undefined;
    const stillCurrent =
      submittedGeneration === resolveRequest &&
      resolved.value !== null &&
      submittedKey === previewKey(resolved.value);
    if (preview && typeof preview === 'object' && stillCurrent) {
      resolved.value = preview as ResolvedLaunch;
      cloneName.value = '';
      clonePreviewKey.value = '';
      error.value = `${(cause as Error).message} Review the fresh settings before saving again.`;
    } else if (stillCurrent) {
      error.value = (cause as Error).message;
    } else {
      cloneNotice.value = `The save from the previous launch selection failed: ${(cause as Error).message}`;
    }
    return;
  } finally {
    cloneBusy.value = false;
  }

  // The clone mutation already committed. Adopt its returned row immediately;
  // a best-effort list refresh below may warn, but can never turn success into
  // a retry that collides with the newly created name.
  rememberProfile(saved);
  const stillCurrent =
    submittedGeneration === resolveRequest &&
    resolved.value !== null &&
    submittedKey === previewKey(resolved.value);
  cloneName.value = '';
  clonePreviewKey.value = '';
  if (stillCurrent) {
    chooseProfile(saved.name);
    cloneNotice.value = `Saved ${saved.name} without changing ${preview.selection.profile}.`;
  } else {
    cloneNotice.value = `Saved ${saved.name}; launch settings have since changed.`;
  }
  try {
    profiles.value = await listProfiles();
  } catch (cause) {
    cloneNotice.value += ` Profile refresh is temporarily unavailable: ${(cause as Error).message}`;
  }
}

function rememberProfile(saved: Profile) {
  profiles.value = [...profiles.value.filter((item) => item.name !== saved.name), saved].sort(
    (left, right) => left.name.localeCompare(right.name),
  );
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
  if (!canCreate.value) {
    error.value = createBlockReason.value;
    await nextTick();
    errorElement.value?.focus();
    return;
  }
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
    // A remote reference must be in the server's managed allowlist before
    // session creation. Direct submission follows the same registration path
    // as choosing “Clone new repo”; a server path travels as `cwd`.
    if (looksLikeRemoteRepo(repoInput)) {
      const known = managedRepos.value.find(
        (candidate) => candidate.slug === repoInput || candidate.remote_url === repoInput,
      );
      const staged =
        registeredRepo.value &&
        (registeredRepo.value.slug === repoInput || registeredRepo.value.remote_url === repoInput)
          ? registeredRepo.value
          : null;
      const registered = known ?? staged ?? (await registerDraftRepo(repoInput));
      body.repo = registered.slug;
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
      lastResolved.value = resolved.value;
    } else if (e instanceof ApiError && e.status === 409) {
      resolved.value = null;
      scheduleResolution();
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
    await nextTick();
    errorElement.value?.focus();
  } finally {
    creating.value = false;
  }
}

function onFormKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || (!event.metaKey && !event.ctrlKey)) return;
  event.preventDefault();
  void create();
}

const createBlockReason = computed(() => {
  if (!repo.value.trim()) return 'Choose a repository before creating the session.';
  if (!(title.value.trim() || goal.value.trim()))
    return 'Add a task title or goal before creating the session.';
  if (branchMode.value === 'existing' && !existingBranch.value.trim())
    return 'Choose the existing branch to reuse.';
  if (scratchError.value) return scratchError.value;
  if (repoRegistrations.value > 0) return 'Wait for repository registration to finish.';
  if (cloneBusy.value) return 'Wait for the new profile to finish saving.';
  if (resolving.value) return 'Wait for launch settings to finish resolving.';
  if (!resolved.value) return resolveError.value || 'Launch settings have not resolved yet.';
  if (!resolved.value.capacity.allowed)
    return `Profile ${resolved.value.selection.profile} is at launch capacity.`;
  if (!resolved.value.valid)
    return resolved.value.errors[0] || 'Resolve the launch settings before creating.';
  if (creating.value) return 'The session is being created.';
  return '';
});
const canCreate = computed(() => !createBlockReason.value);

onActivated(() => void refreshLaunchData());
</script>

<template>
  <!--
    Loom owns repository/branch suggestions, while task fields should start
    clean. Keep browser history completion off at both form and field level:
    product-specific autocomplete tokens create Chrome history buckets instead
    of suppressing them.
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
        Choose a reusable profile, adjust its session settings if needed, and launch.
      </p>
    </div>

    <fieldset
      :disabled="creating"
      class="grid min-h-0 min-w-0 flex-1 gap-6 overflow-auto p-5 disabled:opacity-80 sm:p-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,25rem)]"
    >
      <div class="min-w-0 space-y-5">
        <section class="space-y-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Repository</h3>
          <div class="relative">
            <label for="launch-repository" class="mb-1 block text-xs text-muted">
              Repository - a server path, or a GitHub <span class="font-mono">owner/name</span> to
              clone
              <span v-if="recentRepos.length" class="text-faint">- or pick a recent one</span>
            </label>
            <input
              id="launch-repository"
              v-model="repo"
              @focus="repoFocused = true"
              @input="onRepoInput"
              @blur="repoFocused = false"
              @keydown="onRepoKeydown"
              role="combobox"
              aria-autocomplete="list"
              :aria-expanded="repoFocused && Boolean(repoMatches.length || cloneCandidate)"
              aria-controls="launch-repository-options"
              :aria-activedescendant="
                repoActiveOption >= 0
                  ? optionId(REPOSITORY_OPTION_PREFIX, repoActiveOption)
                  : undefined
              "
              placeholder="owner/name or /home/you/code/project"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            />
            <ul
              id="launch-repository-options"
              v-if="repoFocused && (repoMatches.length || cloneCandidate)"
              role="listbox"
              data-testid="recent-repos"
              class="absolute left-0 right-0 z-20 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
            >
              <li v-if="cloneCandidate">
                <button
                  :id="optionId(REPOSITORY_OPTION_PREFIX, 0)"
                  type="button"
                  role="option"
                  :aria-selected="repoActiveOption === 0"
                  data-testid="clone-repo"
                  :disabled="cloningRepo"
                  class="flex w-full items-center gap-2 px-2 py-1.5 text-left text-accent hover:bg-subtle disabled:opacity-60"
                  :class="{ 'bg-subtle': repoActiveOption === 0 }"
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
              <li v-for="(r, index) in repoMatches" :key="r.repo_root">
                <button
                  :id="optionId(REPOSITORY_OPTION_PREFIX, index + (cloneCandidate ? 1 : 0))"
                  type="button"
                  role="option"
                  :aria-selected="repoActiveOption === index + (cloneCandidate ? 1 : 0)"
                  data-testid="recent-repo"
                  @mousedown.prevent="pickRepo(r.repo_root)"
                  class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-subtle"
                  :class="{
                    'bg-subtle text-fg': repoActiveOption === index + (cloneCandidate ? 1 : 0),
                  }"
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
          <p v-if="repoError" class="text-xs text-block" role="alert">{{ repoError }}</p>
        </section>

        <section class="space-y-3 border-t border-line pt-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">What to build</h3>
          <div>
            <label for="launch-title" class="mb-1 block text-xs text-muted">Title</label>
            <input
              id="launch-title"
              v-model="title"
              placeholder="Health endpoint"
              autocomplete="off"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            />
          </div>
          <div>
            <label for="launch-goal" class="mb-1 block text-xs text-muted">
              Goal - optional; leave blank to start the agent with no prompt
            </label>
            <textarea
              id="launch-goal"
              v-model="goal"
              rows="4"
              placeholder="Add a /health endpoint that returns 200"
              autocomplete="off"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
            ></textarea>
          </div>
        </section>

        <details class="space-y-3 border-t border-line pt-3">
          <summary
            class="cursor-pointer text-2xs font-semibold uppercase tracking-wider text-muted"
          >
            Advanced branch controls
          </summary>
          <div class="pt-3">
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
                <label for="launch-branch-name" class="mb-1 block text-xs text-muted">
                  Name - the worktree (<code>.worktrees/&lt;name&gt;</code>) and branch
                  (<code>weaver/&lt;name&gt;</code>)
                </label>
                <input
                  id="launch-branch-name"
                  v-model="name"
                  @input="nameEdited = true"
                  placeholder="health-endpoint"
                  autocomplete="off"
                  spellcheck="false"
                  class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
                />
              </div>
              <div>
                <label for="launch-base-branch" class="mb-1 block text-xs text-muted">
                  Base branch - fork point (optional)
                </label>
                <input
                  id="launch-base-branch"
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
              <label for="launch-existing-branch" class="mb-1 block text-xs text-muted">
                Existing branch - weaver reuses its worktree if one is checked out
              </label>
              <input
                id="launch-existing-branch"
                v-model="existingBranch"
                @focus="branchFocused = true"
                @input="onBranchInput"
                @blur="branchFocused = false"
                @keydown="onBranchKeydown"
                role="combobox"
                aria-autocomplete="list"
                :aria-expanded="branchFocused && Boolean(branchMatches.length)"
                aria-controls="launch-branch-options"
                :aria-activedescendant="
                  branchActiveOption >= 0
                    ? optionId(BRANCH_OPTION_PREFIX, branchActiveOption)
                    : undefined
                "
                placeholder="feature/foo"
                autocomplete="off"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
              />
              <p v-if="branchesError" class="mt-1 text-xs text-block">{{ branchesError }}</p>
              <ul
                id="launch-branch-options"
                v-if="branchFocused && branchMatches.length"
                role="listbox"
                data-testid="branch-options"
                class="absolute left-0 right-0 z-20 mt-1 max-h-56 overflow-auto rounded border border-line bg-input shadow-lg"
              >
                <li v-for="(b, index) in branchMatches" :key="b.name">
                  <button
                    :id="optionId(BRANCH_OPTION_PREFIX, index)"
                    type="button"
                    role="option"
                    :aria-selected="branchActiveOption === index"
                    data-testid="branch-option"
                    @mousedown.prevent="pickBranch(b)"
                    class="flex w-full items-center justify-between gap-3 px-2 py-1.5 text-left hover:bg-subtle"
                    :class="{ 'bg-subtle text-fg': branchActiveOption === index }"
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
        </details>

        <section class="space-y-3 border-t border-line pt-3">
          <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Scratch files</h3>
          <ScratchPicker
            ref="scratchPicker"
            v-model="scratchFiles"
            :disabled="creating"
            @validation="scratchError = $event"
          />
        </section>
      </div>

      <aside
        class="min-w-0 space-y-4 border-t border-line pt-4 lg:border-l lg:border-t-0 lg:pl-6 lg:pt-0"
      >
        <section class="space-y-3 rounded-md border border-line bg-surface p-3">
          <div class="space-y-1.5 border-b border-line pb-3">
            <div class="flex items-center justify-between gap-3">
              <label class="text-xs font-medium text-fg" for="launch-profile">Profile</label>
              <RouterLink
                to="/settings"
                class="text-xs text-accent hover:underline"
                @click="creating && $event.preventDefault()"
              >
                Manage profiles
              </RouterLink>
            </div>
            <select
              id="launch-profile"
              :value="profile"
              data-testid="launch-profile-picker"
              class="w-full rounded bg-input px-2 py-1.5 text-sm text-fg"
              :disabled="creating"
              @change="chooseProfile(($event.target as HTMLSelectElement).value)"
            >
              <option v-for="candidate in profiles" :key="candidate.name" :value="candidate.name">
                {{ candidate.name }}
              </option>
            </select>
            <p v-if="selectedProfile?.description" class="text-xs text-muted">
              {{ selectedProfile.description }}
            </p>
            <div v-if="selectedProfile" class="flex flex-wrap gap-1 text-2xs text-faint">
              <span class="font-mono">r{{ selectedProfile.revision }}</span>
              <span>· {{ selectedProfile.class }}</span>
              <span v-if="selectedProfile.strict">· strict policy</span>
              <span v-if="selectedProfile.restricted">· restricted</span>
            </div>
          </div>
          <div class="flex items-start justify-between gap-3">
            <div>
              <h3 class="text-xs font-medium text-fg">Profile settings</h3>
              <p class="mt-0.5 text-xs text-faint">
                {{
                  hasLaunchChanges
                    ? 'Changed fields apply to this session.'
                    : 'Edit any field directly; the profile remains unchanged.'
                }}
              </p>
            </div>
            <button
              v-if="hasLaunchChanges"
              type="button"
              class="shrink-0 text-xs text-accent hover:underline"
              data-testid="reset-launch-settings"
              @click="resetLaunchChanges"
            >
              Reset
            </button>
          </div>
          <LaunchOverrides
            v-model="overrides"
            :agents="agents"
            :resolved="resolved"
            :fallback="lastResolved"
            :disabled="Boolean((resolved ?? lastResolved)?.policy.strict)"
          />
          <p v-if="resolving" class="text-xs text-faint" aria-live="polite">Checking settings…</p>
          <ul v-else-if="resolved?.errors.length" class="space-y-1 text-xs text-block">
            <li v-for="message in resolved.errors" :key="message">• {{ message }}</li>
          </ul>
        </section>

        <p v-if="resolveError" class="rounded bg-block-soft p-2 text-xs text-block" role="alert">
          {{ resolveError }}
        </p>

        <section
          v-if="hasLaunchChanges && resolved"
          class="min-w-0 space-y-2 rounded-md border border-line bg-surface p-3"
        >
          <h3 class="text-xs font-medium text-fg">Keep these settings</h3>
          <p class="text-xs text-faint">
            Save the changes as a new profile. The source profile and its policy stay unchanged.
          </p>
          <button
            v-if="!cloneName"
            type="button"
            data-testid="clone-profile-open"
            class="btn-secondary px-2.5 py-1.5 text-xs"
            :disabled="cloneBusy || resolving || !cloneStaticValid"
            @click="beginSaveAsNew"
          >
            Save as new profile…
          </button>
          <template v-else>
            <label class="block text-xs text-muted">
              Profile name
              <input
                v-model="cloneName"
                type="text"
                data-testid="clone-profile-name"
                class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-xs text-fg"
                :disabled="cloneBusy"
              />
            </label>
            <div class="flex flex-wrap gap-2">
              <button
                type="button"
                data-testid="clone-profile"
                class="btn-secondary px-2.5 py-1.5 text-xs"
                :disabled="cloneBusy || resolving || !cloneName.trim()"
                @click="saveAsNewProfile"
              >
                {{ cloneBusy ? 'Saving…' : 'Save profile' }}
              </button>
              <button
                type="button"
                class="px-2.5 py-1.5 text-xs text-muted"
                :disabled="cloneBusy"
                @click="
                  cloneName = '';
                  clonePreviewKey = '';
                "
              >
                Cancel
              </button>
            </div>
          </template>
        </section>
        <p v-if="cloneNotice" class="text-xs text-ok">{{ cloneNotice }}</p>
      </aside>
    </fieldset>

    <p
      v-if="error"
      ref="errorElement"
      tabindex="-1"
      role="alert"
      class="border-t border-line bg-surface px-5 py-2 text-sm text-block outline-none sm:px-8"
    >
      {{ error }}
    </p>

    <div
      class="sticky bottom-0 flex flex-wrap items-center gap-2 border-t border-line bg-surface px-5 py-3 sm:px-8"
    >
      <button
        type="submit"
        data-testid="create-session"
        :disabled="!canCreate"
        class="btn-primary px-3 py-1.5 text-sm font-medium"
      >
        {{ creating ? 'Creating...' : 'Create session' }}
      </button>
      <button
        type="button"
        class="btn-secondary px-3 py-1.5 text-sm font-medium"
        :disabled="creating"
        @click="cancel"
      >
        Cancel
      </button>
      <!-- Keyboard affordance: submit from anywhere in the form (the goal
           textarea swallows a plain Enter) without reaching for the mouse. -->
      <span class="w-full min-w-0 text-2xs text-faint sm:ml-auto sm:w-auto">
        <kbd class="font-mono">{{ metaKeyLabel }}</kbd> + <kbd class="font-mono">Enter</kbd> to
        create
      </span>
      <p
        v-if="!canCreate && !creating"
        class="w-full text-xs text-muted"
        data-testid="create-block-reason"
        aria-live="polite"
      >
        {{ createBlockReason }}
      </p>
    </div>
  </form>
</template>
