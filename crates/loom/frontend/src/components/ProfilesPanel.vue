<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import * as api from '../api';
import type { AgentMetadata, Profile, ProfileInput } from '../types';

const profiles = ref<Profile[]>([]);
const agents = ref<AgentMetadata[]>([]);
const selected = ref('default');
const draft = ref<ProfileInput | null>(null);
const creating = ref(false);
const busy = ref(false);
const error = ref('');
const notice = ref('');
const envName = ref('');
const envValue = ref('');

const current = computed(() => profiles.value.find((profile) => profile.name === selected.value));

function editable(profile: Profile): ProfileInput {
  const {
    revision: _revision,
    created_at: _created,
    updated_at: _updated,
    env: _env,
    ...input
  } = profile;
  // `profile` is a Vue reactive proxy; structuredClone rejects proxies with a
  // DataCloneError. Copy the one nested collection explicitly and keep the
  // editable draft detached from the server snapshot.
  return {
    ...input,
    ambient_allowlist: [...input.ambient_allowlist],
    allowed_tools: [...input.allowed_tools],
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
    const [items, metadata] = await Promise.all([api.listProfiles(), api.listAgents()]);
    profiles.value = items;
    agents.value = metadata.agents;
    choose(
      items.some((item) => item.name === selected.value)
        ? selected.value
        : (items[0]?.name ?? 'default'),
    );
  } catch (e) {
    error.value = (e as Error).message;
  }
}

function add() {
  const agent = agents.value[0]?.kind ?? 'claude';
  selected.value = '';
  creating.value = true;
  draft.value = {
    name: '',
    description: '',
    agent_kind: agent,
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
    restricted: false,
    allowed_tools: [],
  };
}

async function act(fn: () => Promise<void>) {
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
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
  if (!confirm(`Delete profile ${current.value.name}?`)) return;
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
  <div class="grid gap-4 md:grid-cols-[12rem_minmax(0,1fr)]">
    <aside class="overflow-hidden rounded-md border border-line bg-surface self-start">
      <button
        v-for="profile in profiles"
        :key="profile.name"
        class="block w-full border-b border-line px-3 py-2 text-left text-sm last:border-0"
        :class="selected === profile.name ? 'bg-input text-fg' : 'text-muted hover:bg-subtle'"
        @click="choose(profile.name)"
      >
        <span class="block font-medium">{{ profile.name }}</span>
        <span class="text-2xs text-faint">{{ profile.class }} · r{{ profile.revision }}</span>
      </button>
      <button class="block w-full px-3 py-2 text-left text-xs text-accent" @click="add">
        + Add profile
      </button>
    </aside>

    <div class="space-y-4">
      <p v-if="error" class="text-sm text-block">{{ error }}</p>
      <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>
      <template v-if="draft">
        <section class="grid gap-3 rounded-md border border-line bg-surface p-3 sm:grid-cols-2">
          <label class="text-xs"
            >Name
            <input
              v-model="draft.name"
              :disabled="!creating"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="text-xs"
            >Agent
            <select
              data-testid="profile-agent"
              v-model="draft.agent_kind"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            >
              <option v-for="agent in agents" :key="agent.kind" :value="agent.kind">
                {{ agent.label }}
              </option>
            </select>
          </label>
          <label class="text-xs sm:col-span-2"
            >Description
            <input v-model="draft.description" class="mt-1 w-full rounded bg-input px-2 py-1.5" />
          </label>
          <label class="text-xs"
            >Model
            <input
              data-testid="profile-model"
              v-model="draft.model"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="text-xs"
            >Effort
            <input
              data-testid="profile-effort"
              v-model="draft.effort"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="text-xs"
            >Protocol
            <select v-model="draft.protocol" class="mt-1 w-full rounded bg-input px-2 py-1.5">
              <option value="">Agent default</option>
              <option value="acp">ACP</option>
              <option value="terminal">Terminal</option>
            </select>
          </label>
          <label class="text-xs"
            >Mode
            <select
              data-testid="profile-mode"
              v-model="draft.mode"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            >
              <option
                v-for="mode in ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions']"
                :key="mode"
              >
                {{ mode }}
              </option>
            </select>
          </label>
          <label class="text-xs"
            >Class
            <select v-model="draft.class" class="mt-1 w-full rounded bg-input px-2 py-1.5">
              <option value="interactive">Interactive</option>
              <option value="automation">Automation</option>
            </select>
          </label>
          <label class="text-xs"
            >Prelude
            <select v-model="draft.prelude" class="mt-1 w-full rounded bg-input px-2 py-1.5">
              <option value="weaver">Weaver</option>
              <option value="none">None (caller prompt only)</option>
            </select>
          </label>
          <label class="text-xs"
            >Max concurrent (0 = unlimited)
            <input
              v-model.number="draft.max_concurrent"
              type="number"
              min="0"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="flex items-center gap-2 text-xs"
            ><input v-model="draft.strict" type="checkbox" /> Strict (no launch overrides)</label
          >
          <label class="flex items-center gap-2 text-xs"
            ><input v-model="draft.env_clear" type="checkbox" /> Clear ambient environment</label
          >
          <label class="flex items-center gap-2 text-xs"
            ><input v-model="draft.restricted" type="checkbox" /> Restricted automation
            posture</label
          >
          <label class="text-xs"
            >Turn budget (blank = inherit)
            <input
              v-model.number="draft.turn_budget"
              type="number"
              min="0"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="text-xs"
            >Idle archive seconds (blank = inherit)
            <input
              v-model.number="draft.idle_archive_secs"
              type="number"
              min="0"
              class="mt-1 w-full rounded bg-input px-2 py-1.5"
            />
          </label>
          <label class="text-xs sm:col-span-2"
            >Ambient allowlist (comma-separated)
            <input
              :value="draft.ambient_allowlist.join(',')"
              class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono"
              @input="
                draft!.ambient_allowlist = ($event.target as HTMLInputElement).value
                  .split(',')
                  .map((v) => v.trim())
                  .filter(Boolean)
              "
            />
          </label>
          <label class="text-xs sm:col-span-2"
            >Allowed Claude tools (one permission rule per line)
            <textarea
              :value="draft.allowed_tools.join('\n')"
              rows="6"
              class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono"
              @input="
                draft!.allowed_tools = ($event.target as HTMLTextAreaElement).value
                  .split('\n')
                  .map((v) => v.trim())
                  .filter(Boolean)
              "
            />
          </label>
          <div class="flex gap-2 sm:col-span-2">
            <button
              data-testid="profile-save"
              class="btn-primary px-3 py-1.5 text-xs"
              :disabled="busy"
              @click="save"
            >
              Save
            </button>
            <button
              v-if="!creating && selected !== 'default'"
              class="btn-secondary px-3 py-1.5 text-xs"
              :disabled="busy"
              @click="remove"
            >
              Delete
            </button>
          </div>
        </section>

        <section v-if="current && !creating" class="rounded-md border border-line bg-surface p-3">
          <h3 class="mb-1 text-sm font-medium">Profile environment</h3>
          <p class="mb-3 text-xs text-muted">
            Values are write-only and apply on the next launch or real respawn.
          </p>
          <div class="mb-3 flex gap-2">
            <input
              v-model="envName"
              placeholder="NAME"
              class="min-w-0 flex-1 rounded bg-input px-2 py-1.5 font-mono text-xs"
            />
            <input
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
            <code>{{ entry.name }}</code
            ><button class="text-block" @click="removeEnv(entry.name)">Remove</button>
          </div>
          <p v-if="!current.env.length" class="text-xs text-faint">No profile variables.</p>
        </section>
      </template>
    </div>
  </div>
</template>
