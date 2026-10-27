<script setup lang="ts">
import { computed, useId } from 'vue';
import type { AgentMetadata, LaunchOverrides, ResolvedLaunch } from '../types';
import {
  agentOptionsWithCurrent,
  availableAgents,
  isAgentAvailable,
} from '../lib/agentAvailability';
import ModelCombobox from './ModelCombobox.vue';

const props = withDefaults(
  defineProps<{
    agents: AgentMetadata[];
    modelValue: LaunchOverrides;
    resolved: ResolvedLaunch | null;
    fallback?: ResolvedLaunch | null;
    disabled?: boolean;
    /** Which way the option lists prefer to grow. The handoff panel sits on
     *  the right edge, so its lists open leftward. */
    menuPlacement?: 'left' | 'right';
  }>(),
  { menuPlacement: 'right' },
);

const emit = defineEmits<{
  'update:modelValue': [LaunchOverrides];
}>();
const uid = useId();
const settings = computed(() => props.resolved ?? props.fallback ?? null);
const effectiveAgent = computed(
  () =>
    props.modelValue.agent ?? settings.value?.agent ?? availableAgents(props.agents)[0]?.kind ?? '',
);
// The effective agent is always an option, marked when its harness is missing,
// so the control can never display a different agent than the one in effect.
const agentOptions = computed(() => agentOptionsWithCurrent(props.agents, effectiveAgent.value));
const agentChoices = computed(() =>
  agentOptions.value.map((agent) => ({
    id: agent.kind,
    label: `${agent.label}${isAgentAvailable(agent) ? '' : ' — unavailable'}`,
  })),
);
const metadata = computed(() => props.agents.find((agent) => agent.kind === effectiveAgent.value));
const selectedModel = computed(() =>
  metadata.value?.models.find((model) => model.id === value('model')),
);
// A model that scopes effort per model owns the list; one that does not falls
// back to the agent's global list, which is all there is until a catalogue
// fetch fills the per-model choices.
const effortChoices = computed(() =>
  selectedModel.value?.efforts?.length
    ? selectedModel.value.efforts
    : (metadata.value?.efforts ?? []),
);
const modeChoices = ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions'].map((mode) => ({
  id: mode,
  label: mode,
}));
const protocolChoices = computed(() => [
  ...(metadata.value?.supports_acp !== false ? [{ id: 'acp', label: 'ACP' }] : []),
  { id: 'terminal', label: 'Terminal' },
]);
const fastChoices = computed(() => selectedModel.value?.fast ?? []);
function value(field: keyof LaunchOverrides): string {
  return props.modelValue[field] ?? (settings.value?.[field] as string | undefined) ?? '';
}

function changed(field: keyof LaunchOverrides): boolean {
  return Object.prototype.hasOwnProperty.call(props.modelValue, field);
}

function set(field: keyof LaunchOverrides, nextValue: string) {
  const next = { ...props.modelValue, [field]: nextValue };
  if (field === 'agent') {
    // Choosing the agent already in effect is not a change; clearing the
    // selectors here would drop what the profile resolved.
    if (nextValue === effectiveAgent.value) return;
    next.model = '';
    next.effort = '';
    next.fast = '';
  }
  if (field === 'model') {
    const model = metadata.value?.models.find((choice) => choice.id === nextValue);
    // Only prune when the new model states its own choices; an unfetched model
    // says nothing about what the adapter accepts.
    const efforts = model?.efforts ?? [];
    const fast = model?.fast ?? [];
    if (efforts.length && !efforts.some((choice) => choice.id === value('effort'))) {
      next.effort = '';
    }
    if (fast.length && !fast.some((choice) => choice.id === value('fast'))) {
      next.fast = '';
    }
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
    <div class="grid gap-3 sm:grid-cols-2" data-testid="launch-selectors">
      <label class="rounded border border-line bg-input p-2 text-xs sm:col-span-2">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Agent</span>
          <span :class="changed('agent') ? 'text-accent' : 'text-faint'">
            {{ changed('agent') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <ModelCombobox
          :id="`${uid}-agent`"
          :choices="agentChoices"
          :model-value="value('agent')"
          :disabled="locked('agent')"
          :placement="menuPlacement"
          placeholder="Select an agent"
          field-class="bg-surface"
          testid="override-agent"
          @update:model-value="set('agent', $event)"
        />
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Model</span>
          <span :class="changed('model') ? 'text-accent' : 'text-faint'">
            {{ changed('model') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <ModelCombobox
          :id="`${uid}-model`"
          :choices="metadata?.models ?? []"
          :model-value="value('model')"
          :disabled="locked('model')"
          :placement="menuPlacement"
          :allow-custom="metadata?.accepts_raw_model === true"
          empty-label="Agent default"
          field-class="bg-surface"
          testid="override-model"
          @update:model-value="set('model', $event)"
        />
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Effort</span>
          <span :class="changed('effort') ? 'text-accent' : 'text-faint'">
            {{ changed('effort') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <ModelCombobox
          :id="`${uid}-effort`"
          :choices="effortChoices"
          :model-value="value('effort')"
          :disabled="locked('effort')"
          :placement="menuPlacement"
          empty-label="Agent default"
          field-class="bg-surface"
          testid="override-effort"
          @update:model-value="set('effort', $event)"
        />
      </label>
    </div>

    <details class="rounded border border-line bg-input">
      <summary class="cursor-pointer px-2 py-1.5 text-xs text-muted">Advanced runtime</summary>
      <div class="grid gap-3 border-t border-line p-2 sm:grid-cols-2">
        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Fast</span>
            <span :class="changed('fast') ? 'text-accent' : 'text-faint'">
              {{ changed('fast') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <ModelCombobox
            :id="`${uid}-fast`"
            :choices="fastChoices"
            :model-value="value('fast')"
            :disabled="locked('fast')"
            :placement="menuPlacement"
            empty-label="Agent default"
            field-class="bg-surface"
            testid="override-fast"
            @update:model-value="set('fast', $event)"
          />
        </label>

        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Mode</span>
            <span :class="changed('mode') ? 'text-accent' : 'text-faint'">
              {{ changed('mode') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <ModelCombobox
            :id="`${uid}-mode`"
            :choices="modeChoices"
            :model-value="value('mode')"
            :disabled="locked('mode')"
            :placement="menuPlacement"
            placeholder=""
            field-class="bg-surface"
            testid="override-mode"
            @update:model-value="set('mode', $event)"
          />
        </label>

        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Protocol</span>
            <span :class="changed('protocol') ? 'text-accent' : 'text-faint'">
              {{ changed('protocol') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <ModelCombobox
            :id="`${uid}-protocol`"
            :choices="protocolChoices"
            :model-value="value('protocol')"
            :disabled="locked('protocol')"
            :placement="menuPlacement"
            placeholder=""
            field-class="bg-surface"
            testid="override-protocol"
            @update:model-value="set('protocol', $event)"
          />
        </label>

        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Class</span>
            <span :class="changed('class') ? 'text-accent' : 'text-faint'">
              {{ changed('class') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <select
            :id="`${uid}-class`"
            aria-label="Class"
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
