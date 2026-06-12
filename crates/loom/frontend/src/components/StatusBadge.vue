<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ status: string }>();

// Lifecycle (mechanical) status — deliberately NEUTRAL. Lifecycle is not the
// loud signal; the attention axis (AttentionBadge) is. `running` keeps a faint
// accent tint as the one live state worth a glance; everything else is muted
// slate so attention stays the only chromatic thing on the page.
const palette: Record<string, string> = {
  created: 'bg-subtle text-muted',
  launching: 'bg-subtle text-fg',
  running: 'bg-accent/10 text-accent ring-1 ring-inset ring-accent/30',
  orphaned: 'bg-subtle text-muted',
  done: 'bg-subtle text-muted',
  error: 'bg-subtle text-fg',
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
