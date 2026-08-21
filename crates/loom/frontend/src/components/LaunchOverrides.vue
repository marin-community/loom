<script setup lang="ts">
import { computed, useId } from 'vue';
import type { AgentMetadata, LaunchOverrides, ResolvedLaunch } from '../types';
import ModelCombobox from './ModelCombobox.vue';

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
const settings = computed(() => props.resolved ?? props.fallback ?? null);
const availableAgents = computed(() => props.agents.filter((agent) => agent.available !== false));
const effectiveAgent = computed(
  () => props.modelValue.agent ?? settings.value?.agent ?? availableAgents.value[0]?.kind ?? '',
);
const metadata = computed(() => props.agents.find((agent) => agent.kind === effectiveAgent.value));

function value(field: keyof LaunchOverrides): string {
  return props.modelValue[field] ?? (settings.value?.[field] as string | undefined) ?? '';
}

function changed(field: keyof LaunchOverrides): boolean {
  return Object.prototype.hasOwnProperty.call(props.modelValue, field);
}

function set(field: keyof LaunchOverrides, nextValue: string) {
  const next = { ...props.modelValue, [field]: nextValue };
  if (field === 'agent') {
    delete next.model;
    delete next.effort;
  }
  emit('update:modelValue', next);
}

function locked(field: keyof LaunchOverrides): boolean {
  const lockedFields = props.resolved?.locked_fields ?? props.fallback?.locked_fields ?? [];
  return Boolean(props.disabled || lockedFields.includes(field));
}
</script>

<template>
  <div class="space-y-3" data-testid="launch-settings">
    <div class="grid gap-3 sm:grid-cols-2">
      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Agent</span>
          <span :class="changed('agent') ? 'text-accent' : 'text-faint'">
            {{ changed('agent') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-agent`"
          aria-label="Agent"
          :value="value('agent')"
          :disabled="locked('agent')"
          data-testid="override-agent"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('agent', ($event.target as HTMLSelectElement).value)"
        >
          <option v-for="agent in availableAgents" :key="agent.kind" :value="agent.kind">
            {{ agent.label }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Model</span>
          <span :class="changed('model') ? 'text-accent' : 'text-faint'">
            {{ changed('model') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <ModelCombobox
          v-if="metadata?.accepts_raw_model"
          :id="`${uid}-model`"
          :choices="metadata.models"
          :model-value="value('model')"
          :disabled="locked('model')"
          field-class="bg-surface"
          testid="override-model"
          @update:model-value="set('model', $event)"
        />
        <select
          v-else
          :id="`${uid}-model`"
          aria-label="Model"
          :value="value('model')"
          :disabled="locked('model')"
          data-testid="override-model"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('model', ($event.target as HTMLSelectElement).value)"
        >
          <option value="">Agent default</option>
          <option v-for="choice in metadata?.models ?? []" :key="choice.id" :value="choice.id">
            {{ choice.label }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Effort</span>
          <span :class="changed('effort') ? 'text-accent' : 'text-faint'">
            {{ changed('effort') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-effort`"
          aria-label="Effort"
          :value="value('effort')"
          :disabled="locked('effort')"
          data-testid="override-effort"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('effort', ($event.target as HTMLSelectElement).value)"
        >
          <option value="">Agent default</option>
          <option v-for="choice in metadata?.efforts ?? []" :key="choice.id" :value="choice.id">
            {{ choice.label }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Permission mode</span>
          <span :class="changed('mode') ? 'text-accent' : 'text-faint'">
            {{ changed('mode') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-mode`"
          aria-label="Permission mode"
          :value="value('mode')"
          :disabled="locked('mode')"
          data-testid="override-mode"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('mode', ($event.target as HTMLSelectElement).value)"
        >
          <option
            v-for="mode in ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions']"
            :key="mode"
            :value="mode"
          >
            {{ mode }}
          </option>
        </select>
      </label>
    </div>

    <details class="rounded border border-line bg-input">
      <summary class="cursor-pointer px-2 py-1.5 text-xs text-muted">Advanced runtime</summary>
      <div class="grid gap-3 border-t border-line p-2 sm:grid-cols-2">
        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Connection</span>
            <span :class="changed('protocol') ? 'text-accent' : 'text-faint'">
              {{ changed('protocol') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <select
            :id="`${uid}-protocol`"
            aria-label="Connection"
            :value="value('protocol')"
            :disabled="locked('protocol')"
            data-testid="override-protocol"
            class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
            @change="set('protocol', ($event.target as HTMLSelectElement).value)"
          >
            <option value="acp">ACP</option>
            <option value="terminal">Terminal</option>
          </select>
        </label>

        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Session type</span>
            <span :class="changed('class') ? 'text-accent' : 'text-faint'">
              {{ changed('class') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <select
            :id="`${uid}-class`"
            aria-label="Session type"
            :value="value('class')"
            :disabled="locked('class')"
            data-testid="override-class"
            class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
            @change="set('class', ($event.target as HTMLSelectElement).value)"
          >
            <option value="interactive">Interactive</option>
            <option value="automation">Automation</option>
          </select>
        </label>
      </div>
    </details>

    <p
      v-if="(resolved ?? fallback)?.locked_fields.length"
      class="text-xs text-faint"
      data-testid="launch-settings-policy"
    >
      This profile’s policy locks some session settings.
    </p>
  </div>
</template>
