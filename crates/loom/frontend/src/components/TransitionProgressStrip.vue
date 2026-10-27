<script setup lang="ts">
import { computed } from 'vue';
import type { SessionSummary } from '../types';
import { transitionProgress } from '../lib/sessionState';

// The pulsing "the runtime is changing" strip pinned to the foot of a
// conversation — the banner the handoff flow introduced, reused for waking
// and suspending. Which transitions get narrated, and with what words, lives
// in `transitionProgress` so both conversation surfaces (ACP and terminal)
// tell the same story for the same transition.
const props = defineProps<{ session: SessionSummary }>();

const line = computed(() => transitionProgress(props.session));
</script>

<template>
  <div
    v-if="line"
    class="mx-3 mb-3 flex shrink-0 items-center gap-3 rounded border border-info-line/40 bg-info-soft px-3 py-2 text-info"
    :data-testid="line.testid"
    role="status"
    aria-live="polite"
    aria-busy="true"
  >
    <span class="relative flex h-3 w-3 shrink-0" aria-hidden="true">
      <span
        class="absolute inline-flex h-full w-full animate-ping rounded-full bg-info opacity-40"
      ></span>
      <span class="relative inline-flex h-3 w-3 rounded-full bg-info"></span>
    </span>
    <span class="min-w-0">
      <span class="block text-xs font-medium">{{ line.title }}</span>
      <span class="block truncate text-2xs">
        {{ line.detail }}<template v-if="line.paused"> · Session paused</template>
      </span>
    </span>
  </div>
</template>
