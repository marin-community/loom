<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ status: string }>();

const palette: Record<string, string> = {
  created: 'bg-neutral-700 text-neutral-200',
  launching: 'bg-sky-800 text-sky-100',
  working: 'bg-emerald-800 text-emerald-100',
  waiting: 'bg-amber-700 text-amber-100',
  idle: 'bg-neutral-700 text-neutral-200',
  done: 'bg-indigo-800 text-indigo-100',
  error: 'bg-red-800 text-red-100',
};

const cls = computed(() => palette[props.status] ?? 'bg-neutral-700 text-neutral-200');
</script>

<template>
  <span
    :class="cls"
    data-testid="status-badge"
    class="inline-block rounded px-2 py-0.5 text-xs font-medium uppercase tracking-wide"
  >
    {{ status }}
  </span>
</template>
