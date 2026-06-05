<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ status: string }>();

// Lifecycle (mechanical) status. How the agent is *doing* is the separate
// attention axis — see AttentionBadge.vue.
const palette: Record<string, string> = {
  created: 'bg-subtle text-fg',
  launching: 'bg-sky-800 text-sky-100',
  running: 'bg-accent text-accent-fg',
  orphaned: 'bg-amber-900 text-amber-200',
  done: 'bg-indigo-800 text-indigo-100',
  error: 'bg-red-800 text-red-100',
};

const cls = computed(() => palette[props.status] ?? 'bg-subtle text-fg');
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
