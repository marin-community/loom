<script setup lang="ts">
import { computed } from 'vue';
import type { AgentMetadata } from '../types';

const props = withDefaults(
  defineProps<{
    agents: AgentMetadata[];
    agentKind: string;
    model: string;
    effort: string;
    agentGridClass?: string;
    choiceGridClass?: string;
    showAgentBadges?: boolean;
    showAgentCounts?: boolean;
    modelKey?: string;
    effortKey?: string;
    rawModelId?: string;
    rawModelAutocomplete?: string;
  }>(),
  {
    agentGridClass: 'grid gap-2 sm:grid-cols-3',
    choiceGridClass: 'grid gap-3 md:grid-cols-2',
    showAgentBadges: true,
    showAgentCounts: false,
    modelKey: '',
    effortKey: '',
    rawModelId: '',
    rawModelAutocomplete: '',
  },
);

const emit = defineEmits<{
  'update:agent': [string];
  'update:model': [string];
  'update:effort': [string];
}>();

const selectedAgent = computed(() =>
  props.agents.find((agent) => agent.kind === props.agentKind),
);

function updateRawModel(e: Event) {
  emit('update:model', (e.target as HTMLInputElement).value);
}
</script>

<template>
  <div class="space-y-3">
    <div :class="agentGridClass" role="radiogroup" aria-label="Agent">
      <button
        v-for="agentOption in agents"
        :key="agentOption.kind"
        type="button"
        role="radio"
        :aria-checked="agentKind === agentOption.kind"
        class="rounded-md border px-3 py-2 text-left transition-colors"
        :class="
          agentKind === agentOption.kind
            ? 'border-agent-line bg-agent-soft text-fg'
            : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
        "
        @click="emit('update:agent', agentOption.kind)"
      >
        <span class="block text-sm font-medium">{{ agentOption.label }}</span>
        <span v-if="showAgentBadges" class="mt-1 flex flex-wrap gap-1">
          <span
            v-if="agentOption.supports_hooks"
            class="rounded bg-ok-soft px-1.5 py-0.5 text-2xs text-ok"
          >
            hooks
          </span>
          <span
            v-if="agentOption.supports_concierge"
            class="rounded bg-info-soft px-1.5 py-0.5 text-2xs text-info"
          >
            concierge
          </span>
          <span
            v-if="agentOption.accepts_raw_model"
            class="rounded bg-input px-1.5 py-0.5 text-2xs text-faint"
          >
            raw model
          </span>
        </span>
        <span v-if="showAgentCounts" class="mt-0.5 block text-xs text-faint">
          {{ agentOption.models.length || 'Default' }} model{{ agentOption.models.length === 1 ? '' : 's' }}
          <template v-if="agentOption.efforts.length">, {{ agentOption.efforts.length }} effort levels</template>
        </span>
      </button>
    </div>

    <div :class="choiceGridClass">
      <div>
        <div class="mb-1 flex items-center justify-between gap-2">
          <label class="text-2xs font-semibold uppercase tracking-wider text-muted">
            Model
          </label>
          <code v-if="modelKey" class="truncate font-mono text-2xs text-faint">{{ modelKey }}</code>
        </div>
        <div class="flex flex-wrap gap-1.5">
          <button
            type="button"
            class="rounded border px-2.5 py-1 text-xs transition-colors"
            :class="
              !model
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="emit('update:model', '')"
          >
            Default
          </button>
          <button
            v-for="choice in selectedAgent?.models ?? []"
            :key="choice.id"
            type="button"
            class="rounded border px-2.5 py-1 text-xs transition-colors"
            :class="
              model === choice.id
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="emit('update:model', choice.id)"
          >
            {{ choice.label }}
          </button>
        </div>
        <input
          v-if="selectedAgent?.accepts_raw_model"
          :id="rawModelId || undefined"
          :value="model"
          :autocomplete="rawModelAutocomplete || undefined"
          placeholder="custom model"
          class="mt-2 w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
          @input="updateRawModel"
        />
      </div>

      <div>
        <div class="mb-1 flex items-center justify-between gap-2">
          <label class="text-2xs font-semibold uppercase tracking-wider text-muted">
            Effort
          </label>
          <code v-if="effortKey" class="truncate font-mono text-2xs text-faint">{{ effortKey }}</code>
        </div>
        <div class="flex flex-wrap gap-1.5">
          <button
            type="button"
            class="rounded border px-2.5 py-1 text-xs transition-colors"
            :class="
              !effort
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="emit('update:effort', '')"
          >
            Default
          </button>
          <button
            v-for="choice in selectedAgent?.efforts ?? []"
            :key="choice.id"
            type="button"
            class="rounded border px-2.5 py-1 text-xs transition-colors"
            :class="
              effort === choice.id
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="emit('update:effort', choice.id)"
          >
            {{ choice.label }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
