<script setup lang="ts">
import { computed, ref, watch, onMounted } from 'vue';
import type { Session, WeaverEvent, Issue, IssueRefStatus, ArtifactView } from '../types';
import { getArtifact } from '../api';
import SessionActivity from './SessionActivity.vue';
import SessionIssues from './SessionIssues.vue';
import MarkdownView from './MarkdownView.vue';

// The Overview tab: the read-only context for a session — the goal (rendered as
// projected markdown), the pinned `plan` artifact, claimed work + repo backlog,
// and the de-noised activity feed. The PR snapshot lives as a small link in the
// session header rather than as a section here.
// Everything here is authored by the AGENT (the goal, `weaver status`,
// `weaver issue`, `weaver artifact write plan`); the page's writes (acknowledge
// attention, rename, lifecycle) all live in the header, and the live working
// surface (terminal + scratch) is the Terminal tab.
const props = defineProps<{
  ws: Session;
  events: WeaverEvent[];
  format: (ev: WeaverEvent) => string;
  issues: Issue[];
  backlog: Issue[];
}>();

// The goal's `#N` refs aren't projected by the backend (it has no artifact to
// hang a ref map on), so build one client-side from the session's issues — the
// claimed work plus the repo backlog the Overview already loaded. The same chip
// component then renders the goal's references live, keyed by id-as-string.
const goalRefs = computed<Record<string, IssueRefStatus>>(() => {
  const map: Record<string, IssueRefStatus> = {};
  for (const i of [...props.issues, ...props.backlog]) {
    map[String(i.id)] = {
      id: i.id,
      title: i.title,
      status: i.status,
      claimed_branch: i.claimed_branch,
    };
  }
  return map;
});

const hasGoal = computed(() => props.ws.branch.goal.trim().length > 0);

// The pinned `plan` artifact — the well-known document the Overview surfaces.
// Fetch it (latest); absent is the calm empty state (render nothing). Refreshed
// whenever an `artifact_written` shows up in the event feed.
const plan = ref<ArtifactView | null>(null);

async function loadPlan() {
  try {
    plan.value = await getArtifact(props.ws.id, 'plan');
  } catch {
    // 404 (no plan yet) is the empty state, not an error worth surfacing.
    plan.value = null;
  }
}

const planIsMarkdown = computed(() => (plan.value?.meta.kind ?? 'markdown') === 'markdown');

onMounted(loadPlan);
// SSE drives `events`; an `artifact_written` for `plan` means re-fetch. Cheap to
// just reload whenever any artifact_written arrives.
watch(
  () => props.events.filter((e) => e.kind === 'artifact_written').length,
  () => loadPlan(),
);
</script>

<template>
  <div class="space-y-5">
    <!-- Goal — the branch charter, rendered as projected markdown (its `#N`
         issue refs become live status chips from the session's issues). Carries
         `session-goal` for the detail/list specs. -->
    <section
      v-if="hasGoal"
      class="rounded border border-line bg-surface"
      data-testid="session-goal-panel"
    >
      <header class="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
        <div class="text-2xs font-semibold uppercase tracking-wider text-muted">Goal</div>
        <router-link
          :to="`/s/${ws.id}/artifacts/goal`"
          class="ml-auto text-xs text-accent hover:underline"
        >
          Open in Artifacts →
        </router-link>
      </header>
      <div data-testid="session-goal" class="px-1 py-1">
        <MarkdownView
          :id="ws.id"
          path="goal.md"
          :source="ws.branch.goal"
          :refs="goalRefs"
        />
      </div>
    </section>

    <!-- Plan — the pinned well-known `plan` artifact. Markdown renders through
         MarkdownView (projection included); a non-markdown plan shows its
         source. Absent → nothing. -->
    <section
      v-if="plan"
      class="rounded border border-line bg-surface"
      data-testid="session-plan"
    >
      <header class="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
        <div class="text-2xs font-semibold uppercase tracking-wider text-muted">Plan</div>
        <span class="text-sm font-medium text-fg">{{ plan.meta.title || 'plan' }}</span>
        <span class="font-mono text-2xs text-faint">v{{ plan.meta.rev }}</span>
        <router-link
          :to="`/s/${ws.id}/artifacts/plan`"
          class="ml-auto text-xs text-accent hover:underline"
        >
          Open in Artifacts →
        </router-link>
      </header>
      <MarkdownView
        v-if="planIsMarkdown"
        :id="ws.id"
        path="plan.md"
        :source="plan.content"
        :refs="plan.refs.issues"
      />
      <pre v-else class="m-4 overflow-auto rounded bg-code p-3 text-xs text-code-fg">{{ plan.content }}</pre>
    </section>

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
