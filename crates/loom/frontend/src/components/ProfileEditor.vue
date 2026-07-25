<script setup lang="ts">
import { computed, ref, useId, watch } from 'vue';
import type {
  AgentMetadata,
  CloneProfileEnvironment,
  McpRegistry,
  ProfileEnv,
  ProfileInput,
} from '../types';

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
  if (!environment.value || !name || !envValue.value) return;
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

    <div class="grid min-w-0 gap-1">
      <label class="text-xs" :for="`${uid}-model`">Model</label>
      <input
        v-if="selectedAgent?.accepts_raw_model"
        :id="`${uid}-model`"
        v-model="draft.model"
        data-testid="profile-model"
        :list="`${uid}-model-options`"
        :disabled="disabled"
        placeholder="Agent default"
        class="min-w-0 rounded bg-input px-2 py-1.5"
      />
      <datalist v-if="selectedAgent?.accepts_raw_model" :id="`${uid}-model-options`">
        <option
          v-for="model in selectedAgent.models"
          :key="model.id"
          :value="model.id"
          :label="model.label"
        />
      </datalist>
      <select
        v-else
        :id="`${uid}-model`"
        v-model="draft.model"
        data-testid="profile-model"
        :disabled="disabled"
        class="min-w-0 rounded bg-input px-2 py-1.5"
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
        <option value="weaver">Weaver</option>
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

    <fieldset class="min-w-0 space-y-2 rounded border border-line p-2 sm:col-span-2">
      <legend class="px-1 text-xs font-medium">MCP access</legend>
      <div class="flex flex-wrap gap-1.5" role="radiogroup" aria-label="MCP access mode">
        <label
          v-for="mcpMode in ['none', 'all', 'groups'] as const"
          :key="mcpMode"
          class="inline-flex cursor-pointer items-center"
          :class="{
            'cursor-not-allowed opacity-50': disabled || (draft.restricted && mcpMode === 'all'),
          }"
        >
          <input
            v-model="draft.mcp_access.mode"
            type="radio"
            :name="`${uid}-mcp-mode`"
            :value="mcpMode"
            class="mr-1 h-3.5 w-3.5 align-middle"
            :disabled="disabled || (draft.restricted && mcpMode === 'all')"
            @change="
              draft.mcp_access = {
                mode: mcpMode,
                groups: mcpMode === 'groups' ? draft.mcp_access.groups : [],
              }
            "
          />
          <span
            class="rounded border px-2.5 py-1 text-xs capitalize"
            :class="
              draft.mcp_access.mode === mcpMode
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted'
            "
          >
            {{ mcpMode }}
          </span>
        </label>
      </div>
      <div v-if="draft.mcp_access.mode === 'groups'" class="flex flex-wrap gap-2">
        <label
          v-for="group in mcpGroups"
          :key="group.name"
          class="flex items-center gap-1 text-xs"
          :class="{ 'opacity-50': draft.restricted && !group.builtin }"
        >
          <input
            type="checkbox"
            :checked="draft.mcp_access.groups.includes(group.name)"
            :disabled="disabled || (draft.restricted && !group.builtin)"
            @change="
              draft.mcp_access.groups = ($event.target as HTMLInputElement).checked
                ? [...draft.mcp_access.groups, group.name]
                : draft.mcp_access.groups.filter((value) => value !== group.name)
            "
          />
          <code>{{ group.name }}</code>
        </label>
      </div>
      <p class="text-xs text-muted">
        None starts no MCP processes. All includes every enabled builtin and custom MCP. Restricted
        profiles may select trusted builtins only.
      </p>
    </fieldset>

    <div class="grid min-w-0 gap-1 sm:col-span-2">
      <label class="text-xs" :for="`${uid}-permissions`">
        Runtime permissions (one provider-specific rule per line)
      </label>
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
    </div>
    <div v-if="mcpRegistry?.capability_sets.length" class="min-w-0 text-xs sm:col-span-2">
      <div class="mb-1 font-medium">Available trusted MCP capability sets</div>
      <ul class="space-y-1 text-muted">
        <li v-for="set in mcpRegistry.capability_sets" :key="set.name">
          <code>{{ set.name }}</code> — {{ set.description }}
        </li>
      </ul>
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
          :disabled="disabled || !envName.trim() || !envValue"
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
