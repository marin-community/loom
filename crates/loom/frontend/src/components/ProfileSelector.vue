<script setup lang="ts">
import { computed, useId } from 'vue';
import type { Profile } from '../types';

const props = withDefaults(
  defineProps<{
    profiles: Profile[];
    modelValue: string;
    /** Agent kinds whose harness is installed. When given, a profile bound to
     *  any other kind is flagged as unlaunchable on this host. */
    availableAgentKinds?: Set<string>;
    layout?: 'cards' | 'list';
    disabled?: boolean;
  }>(),
  {
    availableAgentKinds: undefined,
    layout: 'cards',
    disabled: false,
  },
);

function agentUnavailable(profile: Profile): boolean {
  return props.availableAgentKinds ? !props.availableAgentKinds.has(profile.agent_kind) : false;
}

const emit = defineEmits<{
  'update:modelValue': [string];
}>();
const groupName = useId();

const classes = computed(() =>
  props.layout === 'cards' ? 'grid gap-2 sm:grid-cols-2 xl:grid-cols-3' : 'space-y-px',
);
</script>

<template>
  <fieldset :class="classes" data-testid="profile-selector">
    <legend class="sr-only">Profile</legend>
    <label
      v-for="profile in profiles"
      :key="profile.name"
      :data-testid="`profile-option-${profile.name}`"
      class="relative block w-full border px-3 py-2 text-left transition-colors"
      :class="[
        layout === 'cards' ? 'rounded-md' : 'first:rounded-t-md last:rounded-b-md',
        modelValue === profile.name
          ? 'border-accent bg-accent/10 text-fg'
          : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg',
        disabled && 'opacity-60',
      ]"
    >
      <input
        class="sr-only"
        type="radio"
        :name="groupName"
        :value="profile.name"
        :checked="modelValue === profile.name"
        :disabled="disabled"
        @change="emit('update:modelValue', profile.name)"
      />
      <span class="flex items-center justify-between gap-2">
        <span class="font-medium">{{ profile.name }}</span>
        <span class="font-mono text-2xs text-faint">r{{ profile.revision }}</span>
      </span>
      <span class="mt-0.5 block truncate text-xs text-faint">
        {{ profile.description || `${profile.agent_kind} · ${profile.class}` }}
      </span>
      <span class="mt-1 block truncate font-mono text-2xs text-muted">
        {{ profile.agent_kind }} · {{ profile.model || 'default model' }} ·
        {{ profile.effort || 'default effort' }}
      </span>
      <span class="mt-1 flex flex-wrap gap-1 text-2xs">
        <span class="rounded bg-subtle px-1.5 py-0.5">{{ profile.mode }}</span>
        <span class="rounded bg-subtle px-1.5 py-0.5">{{ profile.class }}</span>
        <span v-if="profile.strict" class="rounded bg-subtle px-1.5 py-0.5">strict policy</span>
        <span v-if="profile.restricted" class="rounded bg-block-soft px-1.5 py-0.5 text-block">
          restricted
        </span>
        <span
          v-if="agentUnavailable(profile)"
          class="rounded bg-block-soft px-1.5 py-0.5 text-block"
        >
          agent unavailable
        </span>
      </span>
    </label>
  </fieldset>
</template>
