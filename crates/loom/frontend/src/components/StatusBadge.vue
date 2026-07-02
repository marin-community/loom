<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ status: string }>();

// Lifecycle (mechanical) status — a calm, scannable hue per state, kept softer
// than the loud attention axis (SignalChip) so a raised signal still wins the
// eye. `running`/`done` read healthy green, `error` the soft block red, and
// detached/archived states recede to neutral.
const palette: Record<string, string> = {
  created: 'bg-subtle text-muted',
  running: 'bg-ok-soft text-ok ring-1 ring-inset ring-ok-line/30',
  orphaned: 'bg-subtle text-muted',
  done: 'bg-ok-soft text-ok ring-1 ring-inset ring-ok-line/30',
  error: 'bg-block-soft text-block ring-1 ring-inset ring-block-line/30',
  archived: 'bg-subtle text-faint',
};

const cls = computed(() => palette[props.status] ?? 'bg-subtle text-muted');
</script>

<template>
  <span
    :class="cls"
    data-testid="status-badge"
    class="inline-block rounded px-1.5 py-0.5 text-2xs font-medium uppercase tracking-wide font-mono"
  >
    {{ status }}
  </span>
</template>
