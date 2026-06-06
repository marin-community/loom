<script setup lang="ts">
import type { Session, WeaverEvent, Issue } from '../types';
import SessionActivity from './SessionActivity.vue';
import SessionIssues from './SessionIssues.vue';
import SessionPlan from './SessionPlan.vue';

// The Overview tab: the read-only context for a session — the plan (the launch
// goal at minimum, growing into tasks), claimed work + repo backlog, and the
// de-noised activity feed. The PR snapshot now lives as a small link in the
// session header rather than as a section here.
// Everything here is authored by the AGENT (the launch prompt, `weaver
// set-status`, `weaver issue`); the page's writes (acknowledge attention,
// rename, lifecycle) all live in the header, and the live working surface
// (terminal + scratch) is the Terminal tab. This tab is purely "what is this
// session about, and what has it done".
defineProps<{
  ws: Session;
  events: WeaverEvent[];
  format: (ev: WeaverEvent) => string;
  issues: Issue[];
  backlog: Issue[];
}>();
</script>

<template>
  <div class="space-y-5">
    <!-- Plan — one surface for "what this branch is doing": the agent's launch
         goal at minimum, growing into the structured plan (tasks, diagram, live
         status projected from the issue ledger) once one is scaffolded. The goal
         element carries `session-goal` for the detail/list specs. -->
    <SessionPlan :id="ws.id" :goal="ws.branch.goal" />

    <!-- The PR snapshot lives as a small link in the session header now (one
         place you already look), not as a section down here. -->

    <!-- Issues — claimed work + repo backlog (only when there's something). -->
    <SessionIssues v-if="issues.length || backlog.length" :issues="issues" :backlog="backlog" />

    <!-- Activity — de-noised event feed (read-only context). -->
    <section class="rounded border border-line bg-surface p-4">
      <SessionActivity :events="events" :format="format" />
    </section>
  </div>
</template>
