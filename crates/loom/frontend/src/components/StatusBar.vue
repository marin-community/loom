<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRoute } from 'vue-router';
import { unmatchedAutomationRuns, unmatchedRunProjection } from '../lib/automationSessions';
import {
  githubChecksChip,
  githubConflictChip,
  githubReviewChip,
  githubStateChip,
} from '../lib/githubStatus';
import { effectiveAttention } from '../lib/sessionState';
import { useFleet } from '../lib/sessionsStore';
import { useCommandRegistry } from '../lib/commands';
import KeyHint from './KeyHint.vue';

// The workbench status bar — live fleet vitals in one 24px mono strip (see
// docs/loom-ui.md). Read-only API state from the one shared fleet snapshot the
// whole app polls (lib/sessionsStore) — no second poll of its own. Left:
// session + attention counts (the attention segment goes amber and links to the
// filtered list; "all calm" reads a reassuring green). Right: connection dot +
// a ticking clock — the "is this thing live?" glance.
const { sessions, runs, online, focusedSessionId, sessionById } = useFleet();
const { chord: commandChord, hints: commandHints } = useCommandRegistry();
const route = useRoute();
const clock = ref('');

// Automation sessions are ordinary fleet sessions now. Only failed launch
// attempts without a session are counted separately as typed interventions.
const live = computed(() => sessions.value.filter((s) => s.status !== 'archived'));
const history = computed(() => sessions.value.filter((s) => s.status === 'archived'));
const needsMe = computed(
  () =>
    live.value.filter((s) => effectiveAttention(s).level !== 'ok').length +
    unmatchedAutomationRuns(runs.value, sessions.value).filter(
      (run) => unmatchedRunProjection(run) === 'intervention',
    ).length,
);
const contextSession = computed(() => {
  const routeId = route.params.id;
  const id =
    typeof routeId === 'string' && route.path.startsWith(`/s/${routeId}`)
      ? routeId
      : route.path === '/'
        ? focusedSessionId.value
        : '';
  return id ? sessionById(id) : undefined;
});
const github = computed(() => contextSession.value?.branch.github ?? null);
const githubIssue = computed(() => contextSession.value?.github_issue ?? null);
const githubSignals = computed(() => {
  const status = github.value;
  if (!status) return [];
  const state = githubStateChip(status);
  const review = githubReviewChip(status);
  const checks = githubChecksChip(status);
  const signals = [
    state.key === 'OPEN' ? null : state,
    review?.key === 'APPROVED' ? null : review,
    checks?.key === 'passing' ? null : checks,
    githubConflictChip(status),
  ].filter((signal) => signal !== null);
  const compactLabels: Record<string, string> = {
    CHANGES_REQUESTED: 'changes',
    REVIEW_REQUIRED: 'review',
    failing: 'CI fail',
    CONFLICTING: 'conflict',
  };
  return signals.map((signal) => ({
    ...signal,
    label: compactLabels[signal.key] ?? signal.label,
  }));
});
const githubIssueUrl = computed(() => {
  const issue = githubIssue.value;
  return issue ? `https://github.com/${issue.repo}/issues/${issue.number}` : '';
});

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
    class="flex h-6 shrink-0 items-center gap-2 overflow-hidden whitespace-nowrap border-t border-line bg-rail px-3 font-mono text-2xs text-muted sm:gap-4"
  >
    <!-- Counts dim while the server is unreachable — they're the last good
         snapshot, not live truth, and the offline dot on the right says why. -->
    <span
      class="flex shrink-0 items-center gap-2 sm:gap-4"
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
      v-if="github || githubIssue"
      data-testid="status-bar-github"
      class="hidden min-w-0 shrink items-center gap-2 overflow-hidden whitespace-nowrap border-l border-line pl-3 md:flex"
    >
      <a
        v-if="github"
        :href="github.pr_url"
        target="_blank"
        rel="noopener"
        class="shrink-0 text-accent hover:underline"
        data-testid="status-bar-pr"
        :title="`Open PR #${github.pr_number}: ${github.pr_title}`"
      >
        PR #{{ github.pr_number }}
      </a>
      <a
        v-if="githubIssue"
        :href="githubIssueUrl"
        target="_blank"
        rel="noopener"
        class="shrink-0 text-accent hover:underline"
        data-testid="status-bar-issue"
        :title="`Open ${githubIssue.repo} issue #${githubIssue.number}`"
      >
        Issue #{{ githubIssue.number }}
      </a>
      <span
        v-for="signal in githubSignals"
        :key="signal.label"
        :class="signal.cls"
        class="shrink-0"
        data-testid="status-bar-pr-signal"
      >
        {{ signal.label }}
      </span>
    </span>

    <span
      v-if="commandChord"
      data-testid="command-chord"
      class="ml-auto flex items-center gap-1.5 whitespace-nowrap text-accent"
    >
      <KeyHint :keys="commandChord" />
      …
    </span>
    <span
      v-else
      data-testid="command-hints"
      class="ml-auto hidden min-w-0 items-center gap-2 overflow-hidden lg:flex"
    >
      <span
        v-for="command in commandHints"
        :key="command.id"
        class="flex shrink-0 items-center gap-1 text-faint"
      >
        <KeyHint :keys="command.keys[0]" />
        {{ command.label.toLowerCase() }}
      </span>
    </span>

    <span
      class="flex shrink-0 items-center gap-1.5"
      :title="online ? 'Connected' : 'Server unreachable'"
    >
      <span
        class="h-1.5 w-1.5 rounded-full"
        :class="online ? 'bg-accent' : 'bg-block-line'"
        aria-hidden="true"
      ></span>
      {{ online ? 'online' : 'offline' }}
    </span>
    <span class="hidden shrink-0 text-faint sm:inline">{{ clock }}</span>
  </footer>
</template>
