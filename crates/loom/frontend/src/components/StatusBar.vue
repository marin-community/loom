<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { unmatchedAutomationRuns, runNeedsIntervention } from '../lib/automationSessions';
import { effectiveAttention } from '../lib/sessionState';
import { useFleet } from '../lib/sessionsStore';

// The workbench status bar — live fleet vitals in one 24px mono strip (see
// docs/loom-ui.md). Read-only API state from the one shared fleet snapshot the
// whole app polls (lib/sessionsStore) — no second poll of its own. Left:
// session + attention counts (the attention segment goes amber and links to the
// filtered list; "all calm" reads a reassuring green). Right: connection dot +
// a ticking clock — the "is this thing live?" glance.
const { sessions, runs, online } = useFleet();
const clock = ref('');

// Automation sessions are ordinary fleet sessions now. Only failed launch
// attempts without a session are counted separately as typed interventions.
const live = computed(() => sessions.value.filter((s) => s.status !== 'archived'));
const history = computed(() => sessions.value.filter((s) => s.status === 'archived'));
const needsMe = computed(
  () =>
    live.value.filter((s) => effectiveAttention(s).level !== 'ok').length +
    unmatchedAutomationRuns(runs.value, sessions.value).filter(runNeedsIntervention).length,
);

let clockTimer: number | undefined;

function tick() {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, '0');
  clock.value = `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

onMounted(() => {
  tick();
  clockTimer = window.setInterval(tick, 1000);
});
onUnmounted(() => clearInterval(clockTimer));
</script>

<template>
  <footer
    data-testid="status-bar"
    class="flex h-6 shrink-0 items-center gap-2 overflow-hidden border-t border-line bg-rail px-3 font-mono text-2xs text-muted sm:gap-4"
  >
    <!-- Counts dim while the server is unreachable — they're the last good
         snapshot, not live truth, and the offline dot on the right says why. -->
    <span
      class="flex min-w-0 items-center gap-2 sm:gap-4"
      :class="online ? '' : 'opacity-50'"
      :title="online ? '' : 'Last known counts — server unreachable'"
    >
      <router-link to="/" class="hover:text-fg" data-testid="status-bar-sessions">
        {{ live.length }} active session{{ live.length === 1 ? '' : 's' }}
      </router-link>
      <router-link
        v-if="history.length"
        to="/?history=true"
        class="whitespace-nowrap text-faint hover:text-fg"
        data-testid="status-bar-history"
      >
        {{ history.length }} archived
      </router-link>
      <router-link
        v-if="needsMe"
        to="/?view=attention"
        class="flex items-center gap-1.5 text-attn-line hover:text-fg"
        data-testid="status-bar-attention"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-attn-line" aria-hidden="true"></span>
        {{ needsMe }} need{{ needsMe === 1 ? 's' : '' }} attention
      </router-link>
      <span v-else class="flex items-center gap-1.5 text-ok" data-testid="status-bar-attention">
        <span class="h-1.5 w-1.5 rounded-full bg-ok-line" aria-hidden="true"></span>
        all calm
      </span>
    </span>

    <span
      class="ml-auto flex items-center gap-1.5"
      :title="online ? 'Connected' : 'Server unreachable'"
    >
      <span
        class="h-1.5 w-1.5 rounded-full"
        :class="online ? 'bg-accent' : 'bg-block-line'"
        aria-hidden="true"
      ></span>
      {{ online ? 'online' : 'offline' }}
    </span>
    <span class="hidden text-faint sm:inline">{{ clock }}</span>
  </footer>
</template>
