<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import * as api from '../api';
import type { AgentMetadata, McpRegistry, Profile, ProfileInput } from '../types';
import InlineConfirm from './InlineConfirm.vue';
import ProfileEditor from './ProfileEditor.vue';

const profiles = ref<Profile[]>([]);
const agents = ref<AgentMetadata[]>([]);
const mcpRegistry = ref<McpRegistry | null>(null);
const selected = ref('default');
const draft = ref<ProfileInput | null>(null);
const creating = ref(false);
const busy = ref(false);
const error = ref('');
const notice = ref('');
const envName = ref('');
const envValue = ref('');

const availableAgents = computed(() => agents.value.filter((agent) => agent.available !== false));
const availableAgentKinds = computed(
  () => new Set(availableAgents.value.map((agent) => agent.kind)),
);
const visibleProfiles = computed(() =>
  profiles.value.filter((profile) => availableAgentKinds.value.has(profile.agent_kind)),
);

const current = computed(() => profiles.value.find((profile) => profile.name === selected.value));

function editable(profile: Profile): ProfileInput {
  const {
    revision: _revision,
    created_at: _created,
    updated_at: _updated,
    env: _env,
    ...input
  } = profile;
  return {
    ...input,
    expected_revision: profile.revision,
    ambient_allowlist: [...input.ambient_allowlist],
    github_repositories: [...input.github_repositories],
    runtime_permissions: [...input.runtime_permissions],
    mcp_access: { ...input.mcp_access, groups: [...input.mcp_access.groups] },
  };
}

function choose(name: string) {
  selected.value = name;
  const profile = profiles.value.find((item) => item.name === name);
  draft.value = profile ? editable(profile) : null;
  creating.value = false;
  error.value = '';
  notice.value = '';
}

async function load() {
  try {
    const [items, metadata, registry] = await Promise.all([
      api.listProfiles(),
      api.listAgents(),
      api.getMcpRegistry(),
    ]);
    profiles.value = items;
    agents.value = metadata.agents;
    mcpRegistry.value = registry;
    choose(
      visibleProfiles.value.some((item) => item.name === selected.value)
        ? selected.value
        : (visibleProfiles.value[0]?.name ?? 'default'),
    );
  } catch (cause) {
    error.value = (cause as Error).message;
  }
}

function add() {
  selected.value = '';
  creating.value = true;
  draft.value = {
    name: '',
    description: '',
    agent_kind: availableAgents.value[0]?.kind ?? 'claude',
    model: '',
    effort: '',
    protocol: '',
    mode: 'auto',
    class: 'interactive',
    strict: false,
    env_clear: false,
    ambient_allowlist: [],
    idle_archive_secs: null,
    max_concurrent: 0,
    turn_budget: null,
    prelude: 'weaver',
    instructions: '',
    restricted: false,
    github_repositories: [],
    runtime_permissions: [],
    mcp_access: { mode: 'none', groups: [] },
  };
}

async function act(fn: () => Promise<void>) {
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = false;
  }
}

function save() {
  if (!draft.value) return;
  void act(async () => {
    const saved = creating.value
      ? await api.createProfile(draft.value!)
      : await api.updateProfile(selected.value, draft.value!);
    await load();
    choose(saved.name);
    notice.value = `Saved ${saved.name}.`;
  });
}

function remove() {
  if (!current.value || current.value.name === 'default') return;
  void act(async () => {
    await api.deleteProfile(current.value!.name);
    selected.value = 'default';
    await load();
  });
}

function addEnv() {
  if (!current.value || !envName.value.trim()) return;
  void act(async () => {
    await api.setProfileEnv(current.value!.name, envName.value.trim(), envValue.value);
    envName.value = '';
    envValue.value = '';
    await load();
    choose(selected.value);
  });
}

function removeEnv(name: string) {
  if (!current.value) return;
  void act(async () => {
    await api.deleteProfileEnv(current.value!.name, name);
    await load();
    choose(selected.value);
  });
}

onMounted(load);
</script>

<template>
  <section class="min-w-0 overflow-hidden rounded-md border border-line bg-surface">
    <header class="flex flex-wrap items-end gap-2 border-b border-line px-3 py-2">
      <div class="mr-auto min-w-0">
        <h3 class="text-sm font-medium text-fg">Profiles</h3>
        <p class="text-xs text-muted">Reusable agent and policy templates for session launches.</p>
      </div>
      <label class="min-w-48 text-2xs text-muted">
        Profile
        <select
          :value="selected"
          data-testid="profile-picker"
          class="mt-0.5 block w-full rounded bg-input px-2 py-1.5 text-xs text-fg"
          @change="choose(($event.target as HTMLSelectElement).value)"
        >
          <option v-if="creating" value="">New profile</option>
          <option v-for="profile in visibleProfiles" :key="profile.name" :value="profile.name">
            {{ profile.name }} · {{ profile.agent_kind }} ·
            {{ profile.model || 'default model' }}
          </option>
        </select>
      </label>
      <button class="btn-secondary px-2.5 py-1.5 text-xs" @click="add">+ Add profile</button>
    </header>

    <div
      v-if="current && !creating"
      class="flex flex-wrap items-center gap-1.5 border-b border-line bg-input/40 px-3 py-2 text-2xs text-muted"
      data-testid="profile-summary"
    >
      <span class="font-mono text-fg">{{ current.agent_kind }}</span>
      <span>· {{ current.model || 'default model' }}</span>
      <span>· {{ current.effort || 'default effort' }}</span>
      <span>· {{ current.mode }}</span>
      <span>· {{ current.class }}</span>
      <span v-if="current.strict" class="meta-chip">strict policy</span>
      <span v-if="current.restricted" class="meta-chip">restricted</span>
      <span class="ml-auto font-mono text-faint">r{{ current.revision }}</span>
    </div>

    <div class="min-w-0 space-y-4 p-3">
      <p v-if="error" class="text-sm text-block" role="alert">{{ error }}</p>
      <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>
      <template v-if="draft">
        <ProfileEditor
          v-model="draft"
          :agents="availableAgents"
          :mcp-registry="mcpRegistry"
          :name-locked="!creating"
          :disabled="busy"
        />
        <div class="flex flex-wrap items-center gap-2">
          <button
            data-testid="profile-save"
            class="btn-primary px-3 py-1.5 text-xs"
            :disabled="busy || !draft.name.trim()"
            @click="save"
          >
            Save
          </button>
          <InlineConfirm
            v-if="!creating && selected !== 'default'"
            data-testid="profile-delete"
            label="Delete"
            :message="`Delete ${selected}?`"
            :disabled="busy"
            @confirm="remove"
          />
        </div>

        <section v-if="current && !creating" class="rounded-md border border-line bg-surface p-3">
          <h3 class="mb-1 text-sm font-medium">Profile environment</h3>
          <p class="mb-3 text-xs text-muted">
            Values are write-only and apply on the next launch or real respawn.
          </p>
          <div class="mb-3 flex min-w-0 flex-wrap gap-2">
            <label class="sr-only" for="profile-env-name">Environment name</label>
            <input
              id="profile-env-name"
              v-model="envName"
              placeholder="NAME"
              class="min-w-0 flex-1 rounded bg-input px-2 py-1.5 font-mono text-xs"
            />
            <label class="sr-only" for="profile-env-value">Environment value</label>
            <input
              id="profile-env-value"
              v-model="envValue"
              placeholder="value"
              type="password"
              class="min-w-0 flex-1 rounded bg-input px-2 py-1.5 text-xs"
            />
            <button class="btn-primary px-3 py-1.5 text-xs" :disabled="busy" @click="addEnv">
              Set
            </button>
          </div>
          <div
            v-for="entry in current.env"
            :key="entry.name"
            class="flex items-center justify-between border-t border-line py-2 text-xs"
          >
            <code>{{ entry.name }}</code>
            <button class="text-block" @click="removeEnv(entry.name)">Remove</button>
          </div>
          <p v-if="!current.env.length" class="text-xs text-faint">No profile variables.</p>
        </section>
      </template>
    </div>
  </section>
</template>
