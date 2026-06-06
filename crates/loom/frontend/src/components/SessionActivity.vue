<script setup lang="ts">
import { computed, ref } from 'vue';
import type { WeaverEvent } from '../types';

// The Overview activity feed: de-noised to meaningful kinds (status, attention,
// issue_*), newest first, capped at 6 with an "older →" reveal. High-volume
// `hook` events are dropped — they never stream in live, but the initial GET /log
// may include them, so filter defensively.
const props = defineProps<{ events: WeaverEvent[]; format: (ev: WeaverEvent) => string }>();

const MEANINGFUL = new Set([
  'status',
  'attention',
  'issue_added',
  'issue_closed',
  'issue_reopened',
]);

const showAll = ref(false);
const meaningfulEvents = computed(() =>
  props.events.filter((e) => MEANINGFUL.has(e.kind)).slice().reverse(),
);
const visibleEvents = computed(() =>
  showAll.value ? meaningfulEvents.value : meaningfulEvents.value.slice(0, 6),
);
</script>

<template>
  <div>
    <div class="mb-2 text-xs font-medium uppercase tracking-wide text-faint">Recent activity</div>
    <ul class="space-y-1 text-sm">
      <li
        v-for="(ev, i) in visibleEvents"
        :key="ev.id"
        class="stagger-in flex gap-2"
        :style="{ '--i': i }"
      >
        <span class="shrink-0 font-mono text-xs text-faint">{{ ev.created_at.slice(11, 19) }}</span>
        <span class="text-muted">{{ format(ev) }}</span>
      </li>
      <li v-if="!meaningfulEvents.length" class="text-xs text-faint">No activity yet.</li>
    </ul>
    <button
      v-if="meaningfulEvents.length > 6"
      class="mt-1 text-xs text-accent hover:underline"
      @click="showAll = !showAll"
    >
      {{ showAll ? 'show less' : `${meaningfulEvents.length - 6} older →` }}
    </button>
  </div>
</template>
