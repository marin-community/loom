<script setup lang="ts">
import { computed, useId } from 'vue';
import type { AgentMetadata, LaunchOverrides, ResolvedLaunch } from '../types';

const props = defineProps<{
  agents: AgentMetadata[];
  modelValue: LaunchOverrides;
  resolved: ResolvedLaunch | null;
  fallback?: ResolvedLaunch | null;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  'update:modelValue': [LaunchOverrides];
}>();
const uid = useId();
const snapshot = computed(() => props.resolved ?? props.fallback ?? null);

const effectiveAgent = computed(
  () => props.modelValue.agent ?? snapshot.value?.agent ?? props.agents[0]?.kind ?? '',
);
const metadata = computed(() => props.agents.find((agent) => agent.kind === effectiveAgent.value));

function enabled(field: keyof LaunchOverrides): boolean {
  return Object.prototype.hasOwnProperty.call(props.modelValue, field);
}

function fallback(field: keyof LaunchOverrides): string {
  const resolved = snapshot.value;
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
    <fieldset
      v-for="field in ['agent', 'model', 'effort', 'protocol', 'mode', 'class'] as const"
      :key="field"
      class="rounded border border-line bg-input p-2 text-xs"
    >
      <legend class="sr-only capitalize">{{ field }} override</legend>
      <span class="mb-1 flex items-center justify-between gap-2">
        <span class="font-medium capitalize" aria-hidden="true">{{ field }}</span>
        <label :for="`${uid}-${field}-enabled`" class="flex items-center gap-1 text-faint">
          <input
            :id="`${uid}-${field}-enabled`"
            type="checkbox"
            :data-testid="`override-${field}-toggle`"
            :checked="enabled(field)"
            :disabled="locked(field)"
            @change="toggle(field, ($event.target as HTMLInputElement).checked)"
          />
          override
        </label>
      </span>

      <select
        v-if="field === 'agent'"
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.agent ?? snapshot?.agent ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.model ?? snapshot?.model ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.model ?? snapshot?.model ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.effort ?? snapshot?.effort ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.protocol ?? snapshot?.protocol ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.mode ?? snapshot?.mode ?? ''"
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
        :id="`${uid}-${field}-value`"
        :aria-label="`${field} override value`"
        :value="modelValue.class ?? snapshot?.class ?? ''"
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
        Inherits {{ snapshot?.[field] || 'agent default' }}
      </span>
    </fieldset>
  </div>
</template>
