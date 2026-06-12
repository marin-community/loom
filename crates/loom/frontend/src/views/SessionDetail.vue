<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRoute } from 'vue-router';
import { get } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import AgentTerminal from '../components/AgentTerminal.vue';
import ScratchPanel from '../components/ScratchPanel.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import SessionTabs from '../components/SessionTabs.vue';
import SessionOverview from '../components/SessionOverview.vue';

const props = defineProps<{ id: string }>();
const route = useRoute();

const ws = ref<Session | null>(null);
const events = ref<WeaverEvent[]>([]);
const issues = ref<Issue[]>([]);
const backlog = ref<Issue[]>([]);
const error = ref('');

// Work-area tab. Terminal is the default surface — "show me what it's doing" —
// and stays mounted under v-show across both tabs (tearing down the
// WebSocket/xterm/WebGL is the worst thing on a terminal-first page). Files is a
// route, not a tab here, so Monaco never loads just because a session opened. A
// `?tab=overview` query deep-links the Overview tab (the list's open-issue link,
// and the cross-surface return from the file browser).
const initialTab = route.query.tab;
const tab = ref<'terminal' | 'overview'>(initialTab === 'overview' ? 'overview' : 'terminal');

const issueCount = computed(() => issues.value.length + backlog.value.length);

let source: EventSource | null = null;

async function loadSession() {
  ws.value = (await get(`/sessions/${props.id}`)) as Session;
}

async function loadIssues() {
  if (!ws.value) return;
  // The session's own claimed work, plus the repo's unclaimed backlog.
  try {
    issues.value = (await get(`/branches/${ws.value.branch.id}/issues`)) as Issue[];
  } catch {
    // Issues are read-only here; failure is non-fatal for the view.
  }
  try {
    const repo = encodeURIComponent(ws.value.branch.repo_root);
    backlog.value = (await get(`/repos/issues?repo_root=${repo}&scope=backlog`)) as Issue[];
  } catch {
    // Backlog is best-effort context; ignore failures.
  }
}

async function loadAll() {
  try {
    await loadSession();
    events.value = (await get(`/sessions/${props.id}/log`)) as WeaverEvent[];
    await loadIssues();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

function openStream() {
  source = new EventSource(`/api/sessions/${props.id}/events`);
  // `tag` covers every status axis (the agent's attention, an overlooker's
  // triage, any free-form key); a tag write re-fetches the session so the
  // resolved badge and the pill row refresh.
  for (const kind of ['status', 'tag', 'github']) {
    source.addEventListener(kind, (e) => {
      const ev = JSON.parse((e as MessageEvent).data) as WeaverEvent;
      events.value.push(ev);
      loadSession().catch(() => {});
    });
  }
  for (const kind of ['issue_added', 'issue_closed', 'issue_reopened']) {
    source.addEventListener(kind, () => {
      loadIssues().catch(() => {});
    });
  }
  // An artifact write joins the feed; pushing it to `events` also nudges the
  // Overview's pinned-plan watcher to re-fetch the `plan` artifact, and the
  // Artifacts surface refreshes off the same SSE stream itself.
  source.addEventListener('artifact_written', (e) => {
    events.value.push(JSON.parse((e as MessageEvent).data) as WeaverEvent);
  });
}

function eventLine(ev: WeaverEvent): string {
  const d = ev.data || {};
  if (ev.kind === 'status') return `status → ${d.status ?? '?'}`;
  if (ev.kind === 'tag') {
    // `{ key, value, note, by }`; an empty value means the tag was cleared.
    const key = (d.key as string) ?? 'tag';
    const note = d.note ? ` (${d.note})` : '';
    return d.value ? `${key} → ${d.value}${note}` : `${key} cleared`;
  }
  if (ev.kind === 'github')
    return `PR #${d.pr ?? '?'} → ${d.state ?? '?'}${d.checks ? ` · checks ${d.checks}` : ''}`;
  if (ev.kind === 'issue_added') return `issue added: ${d.title ?? ''}`;
  if (ev.kind === 'issue_closed') return `issue closed: #${d.id ?? '?'}`;
  if (ev.kind === 'issue_reopened') return `issue reopened: #${d.id ?? '?'}`;
  if (ev.kind === 'artifact_written')
    return `artifact written: ${d.name ?? '?'}${d.rev ? ` (v${d.rev})` : ''}`;
  return ev.kind;
}

onMounted(() => {
  loadAll();
  openStream();
});
onUnmounted(() => source?.close());
</script>

<template>
  <!-- The page fills the workbench main area exactly (App.vue's main is a
       flex column): header + tabs stay put while the work area (terminal, or
       the scrolling Overview) takes the rest. This is what lets the terminal
       grow to fill instead of a fixed 70vh. -->
  <div v-if="ws" class="flex min-h-[28rem] flex-1 flex-col px-5 py-3">
    <SessionPageHeader :ws="ws" @reload="loadAll" />
    <SessionTabs :tab="tab" :id="props.id" :issue-count="issueCount" @select="tab = $event">
      <!-- Scratch attachments ride the tab row's spare right side (drop a file
           anywhere on the page) so the terminal keeps the vertical space the
           old below-the-terminal strip used to take. -->
      <template #right>
        <ScratchPanel :id="props.id" />
      </template>
    </SessionTabs>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <div class="min-h-0 flex-1">
      <!-- Terminal (default) — the working zone: the live agent fills the space.
           v-show, NEVER v-if: keeping the host in the DOM means AgentTerminal's
           zero-size guard skips the bogus resize while hidden, and its
           ResizeObserver re-fits on return. -->
      <section v-show="tab === 'terminal'" class="h-full">
        <AgentTerminal :id="props.id" />
      </section>

      <!-- Overview — read-only context (goal, issues, activity). Scrolls
           within the work area so the header/tabs stay anchored. -->
      <div v-if="tab === 'overview'" class="h-full overflow-auto pb-1">
        <SessionOverview
          :ws="ws"
          :events="events"
          :format="eventLine"
          :issues="issues"
          :backlog="backlog"
        />
      </div>
    </div>
  </div>
  <p v-else class="px-5 py-3 text-sm text-muted">Loading…</p>
</template>
