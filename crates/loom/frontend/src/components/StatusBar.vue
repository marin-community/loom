<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { get } from '../api';
import type { Session } from '../types';
import { effectiveAttention } from '../lib/sessionState';

// The workbench status bar — live fleet vitals in one 24px mono strip (see
// docs/loom-ui.md). Read-only API state: the same /api/sessions the fleet list
// polls, on a slower tick. Left: session + attention counts (the attention
// segment goes amber and links to the filtered list). Right: connection dot +
// a ticking clock — the "is this thing live?" glance.
const sessions = ref<Session[]>([]);
const online = ref(true);
const clock = ref('');

const live = computed(() => sessions.value.filter((s) => s.status !== 'archived'));
const needsMe = computed(
  () => live.value.filter((s) => effectiveAttention(s).level !== 'ok').length,
);

let pollTimer: number | undefined;
let clockTimer: number | undefined;

async function poll() {
  try {
    sessions.value = (await get('/sessions')) as Session[];
    online.value = true;
  } catch {
    online.value = false;
  }
}

function tick() {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, '0');
  clock.value = `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

onMounted(() => {
  poll();
  tick();
  pollTimer = window.setInterval(poll, 5000);
  clockTimer = window.setInterval(tick, 1000);
});
onUnmounted(() => {
  clearInterval(pollTimer);
  clearInterval(clockTimer);
});
</script>

<template>
  <footer
    data-testid="status-bar"
    class="flex h-6 shrink-0 items-center gap-4 border-t border-line bg-rail px-3 font-mono text-2xs text-muted"
  >
    <!-- Counts dim while the server is unreachable — they're the last good
         snapshot, not live truth, and the offline dot on the right says why. -->
    <span class="flex items-center gap-4" :class="online ? '' : 'opacity-50'" :title="online ? '' : 'Last known counts — server unreachable'">
      <router-link to="/" class="hover:text-fg" data-testid="status-bar-sessions">
        {{ live.length }} session{{ live.length === 1 ? '' : 's' }}
      </router-link>
      <router-link
        v-if="needsMe"
        to="/?filter=attention"
        class="flex items-center gap-1.5 text-attn-line hover:text-fg"
        data-testid="status-bar-attention"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-attn-line" aria-hidden="true"></span>
        {{ needsMe }} need{{ needsMe === 1 ? 's' : '' }} attention
      </router-link>
      <span v-else class="flex items-center gap-1.5 text-faint" data-testid="status-bar-attention">
        <span class="h-1.5 w-1.5 rounded-full bg-faint/60" aria-hidden="true"></span>
        all calm
      </span>
    </span>

    <span class="ml-auto flex items-center gap-1.5" :title="online ? 'Connected' : 'Server unreachable'">
      <span
        class="h-1.5 w-1.5 rounded-full"
        :class="online ? 'bg-accent' : 'bg-block-line'"
        aria-hidden="true"
      ></span>
      {{ online ? 'online' : 'offline' }}
    </span>
    <span class="text-faint">{{ clock }}</span>
  </footer>
</template>
