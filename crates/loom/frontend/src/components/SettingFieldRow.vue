<script setup lang="ts">
import type { SettingView } from '../types';

defineProps<{
  setting: SettingView;
  defaultLabel: string;
  sourceLabel: string;
  isDefault: boolean;
  canReset: boolean;
  dirty: boolean;
  busy: boolean;
}>();

const emit = defineEmits<{
  save: [];
  reset: [];
}>();
</script>

<template>
  <div
    class="grid gap-3 border-b border-line px-3 py-2.5 last:border-0 md:grid-cols-[minmax(12rem,1fr)_minmax(18rem,2fr)]"
  >
    <div class="min-w-0">
      <div class="flex items-center gap-2">
        <label :for="setting.key" class="text-sm font-medium">{{ setting.label }}</label>
        <span
          class="rounded px-1.5 py-0.5 text-2xs"
          :class="isDefault ? 'bg-input text-faint' : 'bg-info-soft text-info'"
        >
          {{ sourceLabel }}
        </span>
      </div>
      <p class="mt-0.5 line-clamp-2 text-xs text-muted">{{ setting.description }}</p>
      <code class="mt-1 block truncate font-mono text-2xs text-faint">{{ setting.key }}</code>
    </div>

    <div class="flex min-w-0 flex-col justify-center gap-1.5">
      <div class="flex min-w-0 items-center gap-2">
        <slot />
        <button
          class="btn-primary px-2.5 py-1 text-xs disabled:opacity-50"
          :disabled="busy || !dirty"
          @click="emit('save')"
        >
          Save
        </button>
        <button
          class="btn-secondary px-2.5 py-1 text-xs disabled:opacity-50"
          :disabled="busy || !canReset"
          :title="`Clear runtime override; inherit: ${defaultLabel}`"
          @click="emit('reset')"
        >
          Reset
        </button>
      </div>
      <p class="text-2xs text-faint">
        Inherited value <code class="font-mono">{{ defaultLabel }}</code>
      </p>
    </div>
  </div>
</template>
