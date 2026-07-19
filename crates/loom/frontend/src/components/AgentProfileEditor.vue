<script setup lang="ts">
import { computed } from 'vue';
import type { AgentMetadata } from '../types';
import AgentRuntimePicker from './AgentRuntimePicker.vue';

const props = defineProps<{
  title: string;
  note: string;
  keys: { agent: string; model: string; effort: string };
  agents: AgentMetadata[];
  agentKind: string;
  model: string;
  effort: string;
  dirty: boolean;
  isDefault: boolean;
  busy: boolean;
}>();

const emit = defineEmits<{
  'update-agent': [string];
  'update-model': [string];
  'update-effort': [string];
  save: [];
  reset: [];
}>();

const selectedAgent = computed(() => props.agents.find((agent) => agent.kind === props.agentKind));

function agentLabel(kind: string): string {
  return props.agents.find((agent) => agent.kind === kind)?.label ?? kind;
}

function choiceLabel(kind: 'model' | 'effort', value: string): string {
  if (!value) return 'Default';
  const choices =
    kind === 'model' ? (selectedAgent.value?.models ?? []) : (selectedAgent.value?.efforts ?? []);
  return choices.find((choice) => choice.id === value)?.label ?? value;
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
  </section>
</template>
