<script setup lang="ts">
import { computed } from 'vue';
import type { AgentMetadata, LaunchOverrides, ResolvedLaunch } from '../types';

const props = defineProps<{
  agents: AgentMetadata[];
  modelValue: LaunchOverrides;
  resolved: ResolvedLaunch | null;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  'update:modelValue': [LaunchOverrides];
}>();

const effectiveAgent = computed(
  () => props.modelValue.agent ?? props.resolved?.agent ?? props.agents[0]?.kind ?? '',
);
const metadata = computed(() => props.agents.find((agent) => agent.kind === effectiveAgent.value));

function enabled(field: keyof LaunchOverrides): boolean {
  return Object.prototype.hasOwnProperty.call(props.modelValue, field);
}

function fallback(field: keyof LaunchOverrides): string {
  const resolved = props.resolved;
  if (!resolved) return '';
  return resolved[field] as string;
}

function toggle(field: keyof LaunchOverrides, checked: boolean) {
  const next = { ...props.modelValue };
  if (checked) next[field] = fallback(field);
  else delete next[field];
  emit('update:modelValue', next);
}

function set(field: keyof LaunchOverrides, value: string) {
  const next = { ...props.modelValue, [field]: value };
  if (field === 'agent') {
    delete next.model;
    delete next.effort;
  }
  emit('update:modelValue', next);
}

function locked(field: keyof LaunchOverrides): boolean {
  return props.disabled || Boolean(props.resolved?.locked_fields.includes(field));
}
</script>

<template>
  <div class="grid gap-3 sm:grid-cols-2" data-testid="launch-overrides">
    <label
      v-for="field in ['agent', 'model', 'effort', 'protocol', 'mode', 'class'] as const"
      :key="field"
      class="rounded border border-line bg-input p-2 text-xs"
    >
      <span class="mb-1 flex items-center justify-between gap-2">
        <span class="font-medium capitalize">{{ field }}</span>
        <span class="flex items-center gap-1 text-faint">
          <input
            type="checkbox"
            :data-testid="`override-${field}-toggle`"
            :checked="enabled(field)"
            :disabled="locked(field)"
            @change="toggle(field, ($event.target as HTMLInputElement).checked)"
          />
          override
        </span>
      </span>

      <select
        v-if="field === 'agent'"
        :value="modelValue.agent ?? resolved?.agent ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option v-for="agent in agents" :key="agent.kind" :value="agent.kind">
          {{ agent.label }}
        </option>
      </select>

      <input
        v-else-if="field === 'model' && metadata?.accepts_raw_model"
        :value="modelValue.model ?? resolved?.model ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        list="launch-model-options"
        placeholder="Agent default"
        class="w-full rounded bg-surface px-2 py-1.5 font-mono disabled:opacity-60"
        @input="set(field, ($event.target as HTMLInputElement).value)"
      />
      <datalist v-if="field === 'model' && metadata?.accepts_raw_model" id="launch-model-options">
        <option v-for="choice in metadata.models" :key="choice.id" :value="choice.id" />
      </datalist>

      <select
        v-else-if="field === 'model'"
        :value="modelValue.model ?? resolved?.model ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option value="">Agent default</option>
        <option v-for="choice in metadata?.models ?? []" :key="choice.id" :value="choice.id">
          {{ choice.label }}
        </option>
      </select>

      <select
        v-else-if="field === 'effort'"
        :value="modelValue.effort ?? resolved?.effort ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option value="">Agent default</option>
        <option v-for="choice in metadata?.efforts ?? []" :key="choice.id" :value="choice.id">
          {{ choice.label }}
        </option>
      </select>

      <select
        v-else-if="field === 'protocol'"
        :value="modelValue.protocol ?? resolved?.protocol ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option value="acp">ACP</option>
        <option value="terminal">Terminal</option>
      </select>

      <select
        v-else-if="field === 'mode'"
        :value="modelValue.mode ?? resolved?.mode ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option
          v-for="mode in ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions']"
          :key="mode"
          :value="mode"
        >
          {{ mode }}
        </option>
      </select>

      <select
        v-else
        :value="modelValue.class ?? resolved?.class ?? ''"
        :disabled="!enabled(field) || locked(field)"
        :data-testid="`override-${field}`"
        class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
        @change="set(field, ($event.target as HTMLSelectElement).value)"
      >
        <option value="interactive">Interactive</option>
        <option value="automation">Automation</option>
      </select>

      <span v-if="locked(field)" class="mt-1 block text-faint">Locked by profile policy.</span>
      <span v-else-if="!enabled(field)" class="mt-1 block truncate text-faint">
        Inherits {{ resolved?.[field] || 'agent default' }}
      </span>
    </label>
  </div>
</template>
