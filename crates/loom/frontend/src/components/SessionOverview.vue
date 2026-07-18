<script setup lang="ts">
import { computed, ref, watch, onMounted } from 'vue';
import type { Session, WeaverEvent, Issue, IssueRefStatus, ArtifactView, ArtifactMeta } from '../types';
import { getArtifact, getArtifacts } from '../api';
import { timeAgo } from '../lib/time';
import { effectiveAttention, lifecycleDot, messageOf } from '../lib/sessionState';
import SessionActivity from './SessionActivity.vue';
import SessionIssues from './SessionIssues.vue';
import MarkdownView from './MarkdownView.vue';

// The Overview tab is the session BRIEF — the pane for coming back to a session
// after time away. It leads with the current state (the agent's `weaver status`
// synopsis), then what there is to read (documents, the links the agent
// surfaced), then the standing context: goal, plan, issues, activity. The PR
// snapshot lives as a small link in the session header rather than as a section
// here. Everything is authored by the AGENT (the goal, `weaver status`,
// `weaver issue`, `weaver artifact write`); the page's writes (acknowledge
// attention, rename, lifecycle) all live in the header, and the live working
// surface (terminal + scratch) is the Terminal tab.
const props = defineProps<{
  ws: Session;
  events: WeaverEvent[];
  format: (ev: WeaverEvent) => string;
  issues: Issue[];
  backlog: Issue[];
}>();

// -- State — the synopsis line ----------------------------------------------

const attention = computed(() => effectiveAttention(props.ws));
const stateMessage = computed(() => messageOf(props.ws) || attention.value.note);
// When the agent last spoke: the attention tag's set_at (stamped on every
// `weaver status`, loud or calm) — else the branch's own updated_at.
const stateWhen = computed(() => {
  const tag = props.ws.branch.tags.find((t) => t.key === 'attention');
  return tag?.set_at || props.ws.branch.updated_at;
});

// -- Docs — every artifact the session published -----------------------------

const docs = ref<ArtifactMeta[]>([]);

async function loadDocs() {
  try {
    // The goal is the charter — it has its own section below, not a list row.
    docs.value = (await getArtifacts(props.ws.id)).filter((a) => a.name !== 'goal');
  } catch {
    docs.value = [];
  }
}

// -- Links — URLs the agent surfaced, plus the canonical GitHub anchors ------

interface BriefLink {
  href: string;
  label: string;
  /** Where it came from — the quiet suffix the row shows. */
  source: string;
}

const URL_RE = /https?:\/\/[^\s)\]>"'`]+/g;

// Strings inside event payloads that can carry prose with URLs in it.
const TEXT_KEYS = ['note', 'text', 'message'] as const;

const links = computed<BriefLink[]>(() => {
  const out: BriefLink[] = [];
  const seen = new Set<string>();
  const push = (href: string, label: string, source: string) => {
    const key = href.replace(/\/+$/, '');
    if (seen.has(key)) return;
    seen.add(key);
    out.push({ href, label, source });
  };

  // The canonical anchors first: the wired GitHub thread, then the PR.
  const wiring = props.ws.branch.tags.find((t) => t.key === 'github')?.value;
  const wired = wiring?.match(/^([\w.-]+\/[\w.-]+)#(\d+)$/);
  if (wired) {
    push(`https://github.com/${wired[1]}/issues/${wired[2]}`, wiring!, 'wired thread');
  }
  const pr = props.ws.branch.github;
  if (pr?.pr_url) {
    push(pr.pr_url, `PR #${pr.pr_number}`, pr.pr_title || 'pull request');
  }

  // Then anything the agent said in its status trail / notes, newest first.
  for (const ev of [...props.events].reverse()) {
    for (const k of TEXT_KEYS) {
      const text = ev.data[k];
      if (typeof text !== 'string') continue;
      for (const raw of text.match(URL_RE) ?? []) {
        const href = raw.replace(/[.,;:]+$/, '');
        push(href, href.replace(/^https?:\/\//, ''), timeAgo(ev.created_at));
      }
    }
  }
  return out.slice(0, 12);
});

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

onMounted(() => {
  loadPlan();
  loadDocs();
});
// SSE drives `events`; an `artifact_written` means the plan or the doc list may
// have moved. Cheap to just reload both whenever one arrives.
watch(
  () => props.events.filter((e) => e.kind === 'artifact_written').length,
  () => {
    loadPlan();
    loadDocs();
  },
);
</script>

<template>
  <div class="space-y-5">
    <!-- State — the synopsis: what the agent last said, when, at what level.
         The one line to read when coming back to a session. -->
    <section
      class="rounded border border-line bg-surface px-4 py-3"
      data-testid="session-state"
    >
      <div class="flex items-baseline gap-2">
        <span
          class="h-2 w-2 shrink-0 self-center rounded-full"
          :class="lifecycleDot(ws)"
          aria-hidden="true"
        ></span>
        <p class="min-w-0 font-serif text-[15px] leading-snug text-fg">
          {{ stateMessage || 'No status reported yet.' }}
        </p>
      </div>
      <div class="mt-1 pl-4 text-2xs text-muted">
        <span v-if="attention.level !== 'ok'" class="font-semibold">{{ attention.level }} · </span>
        <span>{{ timeAgo(stateWhen) }}</span>
      </div>
    </section>

    <!-- Docs — every artifact the session published (the goal stays below as
         the charter). Names link into the artifact viewer. -->
    <section
      v-if="docs.length"
      class="rounded border border-line bg-surface"
      data-testid="session-docs"
    >
      <header class="border-b border-line px-4 py-2.5">
        <div class="text-2xs font-semibold uppercase tracking-wider text-muted">Documents</div>
      </header>
      <ul class="divide-y divide-line">
        <li v-for="d in docs" :key="d.name">
          <router-link
            :to="`/s/${ws.id}/artifacts/${d.name}`"
            class="flex items-baseline gap-2 px-4 py-2 hover:bg-subtle"
          >
            <span class="font-mono text-xs text-accent">{{ d.name }}</span>
            <span class="min-w-0 truncate font-serif text-sm text-fg">{{ d.title }}</span>
            <span class="ml-auto shrink-0 font-mono text-2xs text-faint">v{{ d.rev }}</span>
            <span class="shrink-0 text-2xs text-faint">{{ timeAgo(d.updated_at) }}</span>
          </router-link>
        </li>
      </ul>
    </section>

    <!-- Links — the wired GitHub thread, the PR, and any URL the agent surfaced
         in its status trail, newest first. -->
    <section
      v-if="links.length"
      class="rounded border border-line bg-surface"
      data-testid="session-links"
    >
      <header class="border-b border-line px-4 py-2.5">
        <div class="text-2xs font-semibold uppercase tracking-wider text-muted">Links</div>
      </header>
      <ul class="divide-y divide-line">
        <li v-for="l in links" :key="l.href" class="flex items-baseline gap-2 px-4 py-2">
          <a
            :href="l.href"
            target="_blank"
            rel="noopener"
            class="min-w-0 truncate text-sm text-accent hover:underline"
          >{{ l.label }}</a>
          <span class="ml-auto shrink-0 text-2xs text-faint">{{ l.source }}</span>
        </li>
      </ul>
    </section>

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
