<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { get, post, patch, del } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import AgentTerminal from '../components/AgentTerminal.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import SessionStatusStrip from '../components/SessionStatusStrip.vue';
import SessionTabs from '../components/SessionTabs.vue';
import SessionOverview from '../components/SessionOverview.vue';
import SessionIssues from '../components/SessionIssues.vue';

const props = defineProps<{ id: string }>();
const router = useRouter();
const route = useRoute();

const ws = ref<Session | null>(null);
const events = ref<WeaverEvent[]>([]);
const issues = ref<Issue[]>([]);
const backlog = ref<Issue[]>([]);
const error = ref('');
const notice = ref('');

const busy = ref('');

// Work-area tab. Terminal is the default surface — "show me what it's doing" —
// and stays mounted under v-show across all tabs (tearing down the
// WebSocket/xterm/WebGL is the worst thing on a terminal-first page). Files is
// a route, not a tab here, so Monaco never loads just because a session opened.
// A `?tab=` query deep-links a tab (e.g. the list's open-issue link → issues).
const initialTab = route.query.tab;
const tab = ref<'terminal' | 'overview' | 'issues'>(
  initialTab === 'overview' || initialTab === 'issues' ? initialTab : 'terminal',
);

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
  for (const kind of ['status', 'attention', 'note']) {
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
}

async function act(name: string, fn: () => Promise<void>) {
  busy.value = name;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
}

// Title is the one branch field a human authors (a label for the workstream);
// it is renamed inline from the header. Goal and the status message are written
// by the AGENT (`weaver set-status`, the launch prompt) and are read-only here.
const rename = (title: string) =>
  act('title', async () => {
    await patch(`/sessions/${props.id}`, { title });
    notice.value = 'Title saved.';
    await loadSession();
  });

// The only attention write a human makes: acknowledge the agent's signal by
// clearing it back to `ok`. Leaves the current-state message untouched.
const acknowledge = () =>
  act('acknowledge', async () => {
    await patch(`/sessions/${props.id}`, { attention: 'ok' });
    notice.value = 'Marked OK.';
    await loadSession();
  });

const archive = () =>
  act('archive', async () => {
    if (
      !confirm(
        'Archive this session? This tears down its tmux and removes the worktree, ' +
          'but keeps the branch and its weaver history for reference.',
      )
    )
      return;
    const res = (await post(`/sessions/${props.id}/archive`)) as { branch: string };
    notice.value = `Archived ${res.branch}.`;
    await loadSession();
  });

const remove = () =>
  act('remove', async () => {
    if (!confirm('Remove this session, its worktree and tmux session?')) return;
    await del(`/sessions/${props.id}`);
    router.push('/');
  });

const adopt = () =>
  act('adopt', async () => {
    await post(`/sessions/${props.id}/adopt`);
    notice.value = 'Session adopted — tmux session recreated.';
    await loadSession();
  });

function eventLine(ev: WeaverEvent): string {
  const d = ev.data || {};
  if (ev.kind === 'status') return `status → ${d.status ?? '?'}`;
  if (ev.kind === 'attention')
    return `status → ${d.level ?? '?'}${d.note ? ` (${d.note})` : ''}`;
  if (ev.kind === 'note') return String(d.text ?? '');
  if (ev.kind === 'issue_added') return `issue added: ${d.title ?? ''}`;
  if (ev.kind === 'issue_closed') return `issue closed: #${d.id ?? '?'}`;
  if (ev.kind === 'issue_reopened') return `issue reopened: #${d.id ?? '?'}`;
  return ev.kind;
}

onMounted(() => {
  loadAll();
  openStream();
});
onUnmounted(() => source?.close());
</script>

<template>
  <div v-if="ws">
    <SessionPageHeader :ws="ws" @rename="rename" />
    <SessionStatusStrip :ws="ws" @acknowledge="acknowledge" />

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>

    <SessionTabs :tab="tab" :id="props.id" :issue-count="issueCount" @select="tab = $event" />

    <!-- Terminal (default) — the full-width hero. v-show, NEVER v-if: keeping
         the host in the DOM means AgentTerminal's zero-size guard skips the
         bogus resize while hidden, and its ResizeObserver re-fits on return.
         No wrapper chrome — the terminal IS the surface. -->
    <section v-show="tab === 'terminal'">
      <AgentTerminal :id="props.id" />
    </section>

    <!-- Overview — read-only context (goal, activity, scratch) + lifecycle. -->
    <SessionOverview
      v-if="tab === 'overview'"
      :ws="ws"
      :events="events"
      :format="eventLine"
      :busy="busy"
      @adopt="adopt"
      @archive="archive"
      @remove="remove"
    />

    <!-- Issues — claimed work + repo backlog. -->
    <SessionIssues v-if="tab === 'issues'" :issues="issues" :backlog="backlog" />
  </div>
  <p v-else class="text-muted">Loading…</p>
</template>
