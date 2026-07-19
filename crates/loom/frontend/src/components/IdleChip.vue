<script setup lang="ts">
import { computed } from 'vue';
import type { Tag } from '../types';

// The soothing `idle` mark as a calm, recessive chip: the agent is resting — a
// finished turn or a quiet lull, no one needed. A calm cyan (info) — alive but
// quiet, never the reserved amber/red loud fill, so an idle agent reads as
// restful and the loud attention axis stays the only thing that pops.
// Informational only — the mark is re-stamped each time the agent goes quiet and
// the status watch manages it, so there is no × to clear by hand.
const props = defineProps<{ tag: Tag }>();

const tooltip = computed(() => props.tag.note || 'Idle — the agent is resting; no one is needed.');
</script>

<template>
  <span
    data-testid="idle-chip"
    :title="tooltip"
    class="inline-flex items-center gap-1 rounded border border-info-line/40 px-1.5 py-0.5 text-2xs font-medium uppercase tracking-wide text-info"
  >
    <span class="h-1.5 w-1.5 rounded-full bg-current opacity-70"></span>
    Idle
  </span>
</template>
