<script setup lang="ts">
import { ref } from 'vue';
import { post } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import SessionActivity from './SessionActivity.vue';
import SessionIssues from './SessionIssues.vue';
import GithubStatus from './GithubStatus.vue';
import { timeAgo } from '../lib/time';

// The Overview tab: the read-only context for a session — goal, the GitHub PR
// snapshot, claimed work + repo backlog, and the de-noised activity feed.
// Everything here is authored by the AGENT (the launch prompt, `weaver
// set-status`, `weaver issue`) or polled server-side (the PR snapshot); the
// page's writes (acknowledge attention, rename, lifecycle) all live in the
// header now, and the live working surface (terminal + scratch) is the Terminal
// tab. This tab is purely "what is this session about, and what has it done".
const props = defineProps<{
  ws: Session;
  events: WeaverEvent[];
  format: (ev: WeaverEvent) => string;
  issues: Issue[];
  backlog: Issue[];
}>();

// A header/lifecycle write isn't owned here, but the GitHub snapshot is read-
// only context that belongs with the rest of it — so this tab owns its own
// re-poll and asks the page to reload the session when the snapshot changes.
const emit = defineEmits<{ reload: [] }>();

const refreshing = ref(false);
async function refreshGithub() {
  refreshing.value = true;
  try {
    await post(`/sessions/${props.ws.id}/github`);
    emit('reload');
  } catch {
    // Best-effort — a failed poll leaves the last snapshot in place.
  } finally {
    refreshing.value = false;
  }
}
</script>

<template>
  <div class="space-y-5">
    <!-- Goal — the agent's launch prompt. Read-only. -->
    <section class="rounded border border-line bg-surface p-4">
      <div class="mb-1 text-xs font-medium uppercase tracking-wide text-faint">Goal</div>
      <p
        v-if="ws.branch.goal"
        data-testid="session-goal"
        class="whitespace-pre-wrap text-sm text-fg"
      >{{ ws.branch.goal }}</p>
      <p v-else class="text-sm text-faint">No goal set.</p>
    </section>

    <!-- GitHub — the branch's PR snapshot, polled server-side via `gh`. Shown
         for GitHub-backed repos; the Refresh button forces an immediate re-poll. -->
    <section
      v-if="ws.branch.github || ws.github_repo"
      class="rounded border border-line bg-surface p-4"
    >
      <div class="mb-2 flex items-center justify-between">
        <div class="text-xs font-medium uppercase tracking-wide text-faint">GitHub</div>
        <button
          class="rounded bg-subtle px-2 py-1 text-xs text-muted hover:bg-subtle-hover disabled:opacity-50"
          :disabled="refreshing"
          @click="refreshGithub"
        >
          {{ refreshing ? 'Refreshing…' : 'Refresh' }}
        </button>
      </div>
      <template v-if="ws.branch.github">
        <GithubStatus :gh="ws.branch.github" />
        <p class="mt-2 text-xs text-faint">Snapshot {{ timeAgo(ws.branch.github.fetched_at) }}.</p>
      </template>
      <p v-else class="text-sm text-faint">
        No pull request found for this branch yet. loom polls automatically while
        the session is active.
      </p>
    </section>

    <!-- Issues — claimed work + repo backlog (only when there's something). -->
    <SessionIssues v-if="issues.length || backlog.length" :issues="issues" :backlog="backlog" />

    <!-- Activity — de-noised event feed (read-only context). -->
    <section class="rounded border border-line bg-surface p-4">
      <SessionActivity :events="events" :format="format" />
    </section>
  </div>
</template>
