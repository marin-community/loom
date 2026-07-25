<script setup lang="ts">
import { computed } from 'vue';
import type { Profile } from '../types';

const props = withDefaults(
  defineProps<{
    profiles: Profile[];
    modelValue: string;
    layout?: 'cards' | 'list';
    disabled?: boolean;
  }>(),
  {
    layout: 'cards',
    disabled: false,
  },
);

const emit = defineEmits<{
  'update:modelValue': [string];
}>();

const classes = computed(() =>
  props.layout === 'cards' ? 'grid gap-2 sm:grid-cols-2 xl:grid-cols-3' : 'space-y-px',
);
</script>

<template>
  <div :class="classes" data-testid="profile-selector" role="radiogroup" aria-label="Profile">
    <button
      v-for="profile in profiles"
      :key="profile.name"
      type="button"
      role="radio"
      :aria-checked="modelValue === profile.name"
      :disabled="disabled"
      :data-testid="`profile-option-${profile.name}`"
      class="w-full border px-3 py-2 text-left transition-colors disabled:opacity-60"
      :class="[
        layout === 'cards' ? 'rounded-md' : 'first:rounded-t-md last:rounded-b-md',
        modelValue === profile.name
          ? 'border-accent bg-accent/10 text-fg'
          : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg',
      ]"
      @click="emit('update:modelValue', profile.name)"
    >
      <span class="flex items-center justify-between gap-2">
        <span class="font-medium">{{ profile.name }}</span>
        <span class="font-mono text-2xs text-faint">r{{ profile.revision }}</span>
      </span>
      <span class="mt-0.5 block truncate text-xs text-faint">
        {{ profile.description || `${profile.agent_kind} · ${profile.class}` }}
      </span>
      <span class="mt-1 flex flex-wrap gap-1 text-2xs">
        <span class="rounded bg-subtle px-1.5 py-0.5">{{ profile.agent_kind }}</span>
        <span v-if="profile.strict" class="rounded bg-subtle px-1.5 py-0.5">locked</span>
        <span v-if="profile.restricted" class="rounded bg-block-soft px-1.5 py-0.5 text-block">
          restricted
        </span>
      </span>
    </button>
  </div>
</template>
