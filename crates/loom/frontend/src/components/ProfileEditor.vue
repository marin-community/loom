<script setup lang="ts">
import { computed, ref, useId, watch } from 'vue';
import type {
  AgentMetadata,
  CloneProfileEnvironment,
  McpRegistry,
  ProfileEnv,
  ProfileInput,
} from '../types';
import ModelCombobox from './ModelCombobox.vue';

const props = withDefaults(
  defineProps<{
    agents: AgentMetadata[];
    mcpRegistry: McpRegistry | null;
    nameLocked?: boolean;
    disabled?: boolean;
    sourceEnvironment?: ProfileEnv[];
  }>(),
  { nameLocked: false, disabled: false, sourceEnvironment: () => [] },
);
const draft = defineModel<ProfileInput>({ required: true });
const environment = defineModel<CloneProfileEnvironment | null>('environment');
const uid = useId();
const envName = ref('');
const envValue = ref('');
const envKind = ref<'literal' | 'gcp_secret'>('literal');

const selectedAgent = computed(() =>
  props.agents.find((agent) => agent.kind === draft.value.agent_kind),
);
const mcpGroups = computed(() => {
  const groups = new Map<string, boolean>();
  for (const set of props.mcpRegistry?.capability_sets ?? []) groups.set(set.group, true);
  for (const server of props.mcpRegistry?.custom_servers ?? [])
    if (!groups.has(server.group)) groups.set(server.group, false);
  return [...groups]
    .map(([name, builtin]) => ({ name, builtin }))
    .sort((left, right) => left.name.localeCompare(right.name));
});
const mcpGroupDetails = computed(() =>
  mcpGroups.value.map((group) => {
    const capabilitySets = (props.mcpRegistry?.capability_sets ?? []).filter(
      (set) => set.group === group.name,
    );
    const customServers = (props.mcpRegistry?.custom_servers ?? []).filter(
      (server) =>
        server.group === group.name && server.enabled && server.validation_state === 'ready',
    );
    const adapters = new Set(capabilitySets.map((set) => set.adapter));
    const tools = new Set([
      ...capabilitySets.flatMap((set) => set.tools.map((tool) => `${set.adapter}:${tool}`)),
      ...customServers.flatMap((server) =>
        server.tools.map((tool) => `${server.identity}:${tool}`),
      ),
    ]);
    return {
      ...group,
      descriptions: capabilitySets.map((set) => set.description),
      services: adapters.size + customServers.length,
      tools: tools.size,
    };
  }),
);
const selectedMcpGroups = computed(() => {
  if (draft.value.mcp_access.mode === 'none') return [];
  const available = mcpGroupDetails.value.filter((group) => group.services > 0);
  if (draft.value.mcp_access.mode === 'all') return available;
  const selected = new Set(draft.value.mcp_access.groups);
  return available.filter((group) => selected.has(group.name));
});
const selectedMcpSummary = computed(() => ({
  groups: selectedMcpGroups.value.length,
  services: selectedMcpGroups.value.reduce((count, group) => count + group.services, 0),
  tools: selectedMcpGroups.value.reduce((count, group) => count + group.tools, 0),
}));

function normalizeAgentChoices(metadata = selectedAgent.value) {
  if (!metadata) return;
  if (
    draft.value.model &&
    !metadata.accepts_raw_model &&
    !metadata.models.some((choice) => choice.id === draft.value.model)
  ) {
    draft.value.model = '';
  }
  if (draft.value.effort && !metadata.efforts.some((choice) => choice.id === draft.value.effort)) {
    draft.value.effort = '';
  }
}

function optionalNumber(event: Event): number | null {
  const value = (event.target as HTMLInputElement).value;
  return value === '' ? null : Number(value);
}

function inherited(name: string): boolean {
  return Boolean(environment.value?.inherit && !environment.value.remove.includes(name));
}

function toggleInherited(name: string, include: boolean) {
  if (!environment.value) return;
  environment.value.remove = include
    ? environment.value.remove.filter((entry) => entry !== name)
    : [...new Set([...environment.value.remove, name])];
}

function setEnvironment() {
  const name = envName.value.trim();
  if (!environment.value || !name || (envKind.value === 'gcp_secret' && !envValue.value)) return;
  const proposal = {
    name,
    ...(envKind.value === 'literal' ? { value: envValue.value } : { secret_ref: envValue.value }),
  };
  environment.value.set = [
    ...environment.value.set.filter((entry) => entry.name !== name),
    proposal,
  ];
  environment.value.remove = environment.value.remove.filter((entry) => entry !== name);
  envName.value = '';
  envValue.value = '';
}

watch(selectedAgent, normalizeAgentChoices);
watch(
  () => draft.value.restricted,
  (restricted) => {
    if (!restricted) return;
    draft.value.github_repositories = [];
    if (draft.value.mcp_access.mode === 'all') {
      draft.value.mcp_access = { mode: 'none', groups: [] };
      return;
    }
    if (draft.value.mcp_access.mode === 'groups') {
      const builtins = new Set(
        mcpGroups.value.filter((group) => group.builtin).map((group) => group.name),
      );
      draft.value.mcp_access.groups = draft.value.mcp_access.groups.filter((group) =>
        builtins.has(group),
      );
    }
  },
);
</script>

<template>
  <section
    class="grid min-w-0 gap-3 rounded-md border border-line bg-surface p-3 sm:grid-cols-2"
    data-testid="profile-editor"
  >
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-name`">Name</label>
      <input
        :id="`${uid}-name`"
        v-model="draft.name"
        :disabled="disabled || nameLocked"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      />
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-agent`">Agent</label>
      <select
        :id="`${uid}-agent`"
        v-model="draft.agent_kind"
        data-testid="profile-agent"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option v-for="agent in agents" :key="agent.kind" :value="agent.kind">
          {{ agent.label }}
        </option>
      </select>
    </div>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-description`">Description</label>
      <input
        :id="`${uid}-description`"
        v-model="draft.description"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      />
    </div>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-instructions`">Opening instructions</label>
      <textarea
        :id="`${uid}-instructions`"
        v-model="draft.instructions"
        data-testid="profile-instructions"
        rows="8"
        :disabled="disabled"
        placeholder="Organization workflow and response conventions for sessions using this profile"
        class="min-w-0 rounded bg-input px-2 py-1.5 font-mono text-xs"
      />
      <p class="text-xs text-muted">
        Appended to the first prompt for user, Slack, GitHub, delegated, and automation launches. Do
        not put secrets here.
      </p>
    </div>

    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-model`">Model</label>
      <ModelCombobox
        v-if="selectedAgent?.accepts_raw_model"
        :id="`${uid}-model`"
        v-model="draft.model"
        :choices="selectedAgent.models"
        :disabled="disabled"
        field-class="bg-input"
        testid="profile-model"
      />
      <select
        v-else
        :id="`${uid}-model`"
        v-model="draft.model"
        data-testid="profile-model"
        :disabled="disabled"
        class="min-w-0 w-full rounded bg-input px-2 py-1.5"
      >
        <option value="">Agent default</option>
        <option v-for="model in selectedAgent?.models ?? []" :key="model.id" :value="model.id">
          {{ model.label }}
        </option>
      </select>
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-effort`">Effort</label>
      <select
        :id="`${uid}-effort`"
        v-model="draft.effort"
        data-testid="profile-effort"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option value="">Agent default</option>
        <option v-for="effort in selectedAgent?.efforts ?? []" :key="effort.id" :value="effort.id">
          {{ effort.label }}
        </option>
      </select>
    </div>

    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-protocol`">Protocol</label>
      <select
        :id="`${uid}-protocol`"
        v-model="draft.protocol"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option value="">Agent default</option>
        <option value="acp">ACP</option>
        <option value="terminal">Terminal</option>
      </select>
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-mode`">Mode</label>
      <select
        :id="`${uid}-mode`"
        v-model="draft.mode"
        data-testid="profile-mode"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option
          v-for="mode in ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions']"
          :key="mode"
        >
          {{ mode }}
        </option>
      </select>
    </div>

    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-class`">Class</label>
      <select
        :id="`${uid}-class`"
        v-model="draft.class"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option value="interactive">Interactive</option>
        <option value="automation">Automation</option>
      </select>
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-prelude`">Prelude</label>
      <select
        :id="`${uid}-prelude`"
        v-model="draft.prelude"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      >
        <option value="weaver">Loom</option>
        <option value="none">None (caller prompt only)</option>
      </select>
    </div>

    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-max`">Max concurrent (0 = unlimited)</label>
      <input
        :id="`${uid}-max`"
        v-model.number="draft.max_concurrent"
        type="number"
        min="0"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      />
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-turns`">Turn budget (blank = inherit)</label>
      <input
        :id="`${uid}-turns`"
        :value="draft.turn_budget ?? ''"
        type="number"
        min="0"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
        @input="draft.turn_budget = optionalNumber($event)"
      />
    </div>
    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-idle`">Idle archive seconds (blank = inherit)</label>
      <input
        :id="`${uid}-idle`"
        :value="draft.idle_archive_secs ?? ''"
        type="number"
        min="0"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
        @input="draft.idle_archive_secs = optionalNumber($event)"
      />
    </div>

    <label class="flex items-center gap-2 text-xs">
      <input v-model="draft.strict" type="checkbox" :disabled="disabled" />
      Strict (no launch overrides)
    </label>
    <label class="flex items-center gap-2 text-xs">
      <input v-model="draft.env_clear" type="checkbox" :disabled="disabled" />
      Clear ambient environment
    </label>
    <label class="flex items-center gap-2 text-xs sm:col-span-2">
      <input v-model="draft.restricted" type="checkbox" :disabled="disabled" />
      Restricted automation posture
    </label>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-ambient`">Ambient allowlist (comma-separated)</label>
      <input
        :id="`${uid}-ambient`"
        :value="draft.ambient_allowlist.join(',')"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5 font-mono"
        @input="
          draft.ambient_allowlist = ($event.target as HTMLInputElement).value
            .split(',')
            .map((value) => value.trim())
            .filter(Boolean)
        "
      />
    </div>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-github-repositories`">
        GitHub App repositories (owner/name, one per line)
      </label>
      <textarea
        :id="`${uid}-github-repositories`"
        :value="draft.github_repositories.join('\n')"
        :disabled="disabled || draft.restricted"
        rows="3"
        class="min-w-0 rounded bg-input px-2 py-1.5 font-mono"
        @input="
          draft.github_repositories = ($event.target as HTMLTextAreaElement).value
            .split('\n')
            .map((value) => value.trim())
            .filter(Boolean)
        "
      />
    </div>

    <fieldset class="min-w-0 space-y-3 rounded border border-line p-3 sm:col-span-2">
      <legend class="px-1 text-xs font-medium">
        <span class="sr-only">MCP access</span><span aria-hidden="true">Loom tools</span>
      </legend>
      <p class="text-xs text-muted">
        Choose which Loom resources and connected services this profile exposes to the agent.
        Selection is stamped onto each new session.
      </p>
      <div class="grid gap-2 sm:grid-cols-3" role="radiogroup" aria-label="MCP access">
        <label
          v-for="option in [
            {
              value: 'none' as const,
              title: 'No Loom tools',
              detail: 'Start no profile-managed tool services.',
            },
            {
              value: 'all' as const,
              title: 'All available',
              detail: 'Include every enabled built-in and custom service.',
            },
            {
              value: 'groups' as const,
              title: 'Choose capabilities',
              detail: 'Select only the resource families this profile needs.',
            },
          ]"
          :key="option.value"
          class="flex cursor-pointer gap-2 rounded border p-2"
          :class="{
            'border-accent bg-accent/10': draft.mcp_access.mode === option.value,
            'border-line bg-input': draft.mcp_access.mode !== option.value,
            'cursor-not-allowed opacity-50':
              disabled || (draft.restricted && option.value === 'all'),
          }"
        >
          <input
            v-model="draft.mcp_access.mode"
            type="radio"
            :name="`${uid}-mcp-mode`"
            :value="option.value"
            :aria-label="option.value"
            class="mt-0.5 h-3.5 w-3.5"
            :disabled="disabled || (draft.restricted && option.value === 'all')"
            @change="
              draft.mcp_access = {
                mode: option.value,
                groups: option.value === 'groups' ? draft.mcp_access.groups : [],
              }
            "
          />
          <span class="grid gap-0.5">
            <span class="text-xs font-medium text-fg">{{ option.title }}</span>
            <span class="text-2xs text-muted">{{ option.detail }}</span>
          </span>
        </label>
      </div>
      <div v-if="draft.mcp_access.mode === 'groups'" class="grid gap-2 sm:grid-cols-2">
        <label
          v-for="group in mcpGroupDetails"
          :key="group.name"
          class="flex gap-2 rounded border border-line bg-input p-2 text-xs"
          :class="{ 'cursor-not-allowed opacity-50': draft.restricted && !group.builtin }"
        >
          <input
            type="checkbox"
            :checked="draft.mcp_access.groups.includes(group.name)"
            :disabled="disabled || (draft.restricted && !group.builtin)"
            class="mt-0.5"
            @change="
              draft.mcp_access.groups = ($event.target as HTMLInputElement).checked
                ? [...draft.mcp_access.groups, group.name]
                : draft.mcp_access.groups.filter((value) => value !== group.name)
            "
          />
          <span class="grid min-w-0 gap-0.5">
            <span class="flex items-center gap-1.5">
              <span class="font-medium text-fg">{{ group.name }}</span>
              <span v-if="!group.builtin" class="meta-chip">custom</span>
            </span>
            <span class="text-2xs text-muted">
              {{ group.services }} {{ group.services === 1 ? 'service' : 'services' }} ·
              {{ group.tools }} {{ group.tools === 1 ? 'tool' : 'tools' }}
            </span>
            <span v-if="group.descriptions.length" class="text-2xs text-faint">
              {{ group.descriptions.join(' ') }}
            </span>
          </span>
        </label>
      </div>
      <div class="rounded bg-input/60 px-2.5 py-2 text-xs">
        <template v-if="selectedMcpSummary.tools">
          New sessions get <strong>{{ selectedMcpSummary.tools }} tools</strong> from
          {{ selectedMcpSummary.services }}
          {{ selectedMcpSummary.services === 1 ? 'service' : 'services' }} across
          {{ selectedMcpSummary.groups }}
          {{ selectedMcpSummary.groups === 1 ? 'capability' : 'capabilities' }}.
        </template>
        <template v-else>No Loom tool services will start for new sessions.</template>
      </div>
      <p v-if="draft.restricted" class="text-xs text-muted">
        Restricted profiles can use explicitly selected trusted built-in capabilities only.
      </p>
    </fieldset>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-permissions`">Provider permissions</label>
      <textarea
        :id="`${uid}-permissions`"
        :value="draft.runtime_permissions.join('\n')"
        rows="3"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5 font-mono"
        @input="
          draft.runtime_permissions = ($event.target as HTMLTextAreaElement).value
            .split('\n')
            .map((value) => value.trim())
            .filter(Boolean)
        "
      />
      <p class="text-2xs text-muted">
        Native agent permission rules, one per line. Loom tool access is controlled separately
        above.
      </p>
    </div>
    <fieldset
      v-if="environment"
      class="min-w-0 space-y-3 rounded border border-line p-2 sm:col-span-2"
      data-testid="profile-environment-editor"
    >
      <legend class="px-1 text-xs font-medium">Profile environment</legend>
      <label class="flex items-center gap-2 text-xs">
        <input v-model="environment.inherit" type="checkbox" :disabled="disabled" />
        Start with the source profile’s write-only values
      </label>
      <div v-if="sourceEnvironment.length" class="grid gap-1">
        <label
          v-for="entry in sourceEnvironment"
          :key="entry.name"
          class="flex min-w-0 items-center gap-2 text-xs"
        >
          <input
            type="checkbox"
            :checked="inherited(entry.name)"
            :disabled="disabled || !environment.inherit"
            @change="toggleInherited(entry.name, ($event.target as HTMLInputElement).checked)"
          />
          <code class="min-w-0 truncate">{{ entry.name }}</code>
          <span class="text-faint">
            {{ entry.source === 'gcp_secret' ? entry.secret_ref : 'write-only literal' }}
          </span>
        </label>
      </div>
      <div class="grid min-w-0 gap-2 sm:grid-cols-[minmax(0,1fr)_8rem_minmax(0,1.4fr)_auto]">
        <label class="sr-only" :for="`${uid}-env-name`">Environment name</label>
        <input
          :id="`${uid}-env-name`"
          v-model="envName"
          placeholder="NAME"
          class="min-w-0 rounded bg-input px-2 py-1.5 font-mono text-xs"
          :disabled="disabled"
        />
        <label class="sr-only" :for="`${uid}-env-kind`">Environment value type</label>
        <select
          :id="`${uid}-env-kind`"
          v-model="envKind"
          class="min-w-0 rounded bg-input px-2 py-1.5 text-xs"
          :disabled="disabled"
        >
          <option value="literal">Literal</option>
          <option value="gcp_secret">Secret ref</option>
        </select>
        <label class="sr-only" :for="`${uid}-env-value`">Environment value</label>
        <input
          :id="`${uid}-env-value`"
          v-model="envValue"
          type="password"
          :placeholder="envKind === 'literal' ? 'write-only value' : 'projects/…/versions/…'"
          class="min-w-0 rounded bg-input px-2 py-1.5 text-xs"
          :disabled="disabled"
        />
        <button
          type="button"
          class="btn-secondary px-2.5 py-1.5 text-xs"
          :disabled="disabled || !envName.trim() || (envKind === 'gcp_secret' && !envValue)"
          @click="setEnvironment"
        >
          Set
        </button>
      </div>
      <div
        v-for="entry in environment.set"
        :key="entry.name"
        class="flex min-w-0 items-center justify-between gap-2 border-t border-line pt-2 text-xs"
      >
        <span class="min-w-0 truncate">
          <code>{{ entry.name }}</code>
          <span class="ml-2 text-faint">{{
            entry.secret_ref ? 'secret reference' : 'literal'
          }}</span>
        </span>
        <button
          type="button"
          class="text-block"
          :disabled="disabled"
          @click="environment.set = environment.set.filter((value) => value.name !== entry.name)"
        >
          Remove edit
        </button>
      </div>
      <p class="text-xs text-faint">
        Secret values are never read back. These changes commit with the new template or not at all.
      </p>
    </fieldset>
  </section>
</template>
