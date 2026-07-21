<script setup lang="ts">
import { computed } from 'vue';
import type { AgentMetadata } from '../types';
import AgentRuntimePicker from './AgentRuntimePicker.vue';

const props = defineProps<{
  title: string;
  note: string;
  keys: { agent: string; model: string; effort: string; mode: string };
  agents: AgentMetadata[];
  agentKind: string;
  model: string;
  effort: string;
  mode: string;
  dirty: boolean;
  isDefault: boolean;
  busy: boolean;
}>();

const emit = defineEmits<{
  'update-agent': [string];
  'update-model': [string];
  'update-effort': [string];
  'update-mode': [string];
  save: [];
  reset: [];
}>();

const selectedAgent = computed(() => props.agents.find((agent) => agent.kind === props.agentKind));

const permissionModes = [
  {
    id: 'auto',
    label: 'Auto',
    description: 'Allow routine work and ask before risky actions.',
  },
  {
    id: 'default',
    label: 'Ask',
    description: 'Use the provider’s standard permission prompts.',
  },
  {
    id: 'acceptEdits',
    label: 'Accept edits',
    description: 'Allow file edits while asking for other actions.',
  },
  {
    id: 'plan',
    label: 'Plan only',
    description: 'Keep the agent read-only while it plans.',
  },
  {
    id: 'bypassPermissions',
    label: 'Always allow',
    description: 'Run without permission prompts.',
  },
];

function agentLabel(kind: string): string {
  return props.agents.find((agent) => agent.kind === kind)?.label ?? kind;
}

function choiceLabel(kind: 'model' | 'effort', value: string): string {
  if (!value) return 'Default';
  const choices =
    kind === 'model' ? (selectedAgent.value?.models ?? []) : (selectedAgent.value?.efforts ?? []);
  return choices.find((choice) => choice.id === value)?.label ?? value;
}

function permissionLabel(value: string): string {
  return permissionModes.find((mode) => mode.id === value)?.label ?? value;
}
</script>

<template>
  <section class="rounded-md border border-line bg-surface">
    <div class="flex flex-wrap items-start gap-3 border-b border-line px-3 py-2">
      <div class="min-w-0">
        <h3 class="text-sm font-semibold">{{ title }}</h3>
        <p class="text-xs text-muted">{{ note }}</p>
      </div>
      <div class="ml-auto flex items-center gap-2">
        <span class="rounded bg-agent-soft px-2 py-1 text-xs text-agent">
          {{ agentLabel(agentKind) }}
          <template v-if="model"> · {{ choiceLabel('model', model) }}</template>
          <template v-if="effort"> · {{ choiceLabel('effort', effort) }}</template>
          · {{ permissionLabel(mode) }} permissions
        </span>
        <button
          class="btn-primary px-2.5 py-1 text-xs disabled:opacity-50"
          :disabled="busy || !dirty"
          @click="emit('save')"
        >
          Save
        </button>
        <button
          class="btn-secondary px-2.5 py-1 text-xs disabled:opacity-50"
          :disabled="busy || isDefault"
          @click="emit('reset')"
        >
          Reset
        </button>
      </div>
    </div>

    <div class="px-3 py-3">
      <AgentRuntimePicker
        :agents="agents"
        :agent-kind="agentKind"
        :model="model"
        :effort="effort"
        :model-key="keys.model"
        :effort-key="keys.effort"
        @update:agent="emit('update-agent', $event)"
        @update:model="emit('update-model', $event)"
        @update:effort="emit('update-effort', $event)"
      />
    </div>

    <div class="border-t border-line px-3 py-3">
      <div class="mb-2 flex items-baseline justify-between gap-3">
        <div>
          <h4 class="text-xs font-medium">Default permissions</h4>
          <p class="text-2xs text-muted">Used for new ACP sessions and handoffs.</p>
        </div>
        <code class="font-mono text-2xs text-faint">{{ keys.mode }}</code>
      </div>
      <div class="grid gap-1.5 sm:grid-cols-2 lg:grid-cols-5" data-testid="agent-mode-picker">
        <button
          v-for="option in permissionModes"
          :key="option.id"
          type="button"
          class="rounded border px-2.5 py-2 text-left"
          :class="
            mode === option.id
              ? 'border-accent bg-accent text-accent-fg'
              : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
          "
          :data-active="mode === option.id"
          @click="emit('update-mode', option.id)"
        >
          <span class="block text-xs font-medium">{{ option.label }}</span>
          <span class="mt-0.5 block text-2xs opacity-80">{{ option.description }}</span>
        </button>
      </div>
    </div>
  </section>
</template>
