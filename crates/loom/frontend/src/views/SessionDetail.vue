<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRoute } from 'vue-router';
import { get, ideInfo } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import SessionTerminals from '../components/SessionTerminals.vue';
import IdeFrame from '../components/IdeFrame.vue';
import ScratchPanel from '../components/ScratchPanel.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import SessionTabs from '../components/SessionTabs.vue';
import SessionOverview from '../components/SessionOverview.vue';
import SessionConversation from '../components/SessionConversation.vue';

const props = defineProps<{ id: string }>();
const route = useRoute();

const ws = ref<Session | null>(null);
const events = ref<WeaverEvent[]>([]);
const issues = ref<Issue[]>([]);
const backlog = ref<Issue[]>([]);
const error = ref('');

// Work-area tab. Terminal is the default surface — "show me what it's doing" —
// and stays mounted under v-show across both tabs (tearing down the
// WebSocket/xterm/WebGL is the worst thing on a terminal-first page). A
// `?tab=overview` query deep-links the Overview tab (e.g. the list's open-issue
// link). Files are no longer a tab — they live in the embedded editor panel.
const initialTab = route.query.tab;
type WorkTab = 'terminal' | 'overview' | 'conversation';
const tab = ref<WorkTab>(
  initialTab === 'overview' || initialTab === 'conversation' ? initialTab : 'terminal',
);

const issueCount = computed(() => issues.value.length + backlog.value.length);

// --- Embedded editor (code-server) side panel ------------------------------
// The editor lives in a resizable panel pulled in from the right, beside the
// live terminal. It is closed by default and mounted only when open, so opening
// it is what lazily spawns the session's code-server — a plain session-open
// never does. `ideEnabled` gates the whole affordance on the server setting.
const ideEnabled = ref(false);
const ideOpen = ref(false);
const MIN_IDE_WIDTH = 360;
function loadIdeWidth(): number {
  const v = Number(localStorage.getItem('loom.ideWidth'));
  return Number.isFinite(v) && v >= MIN_IDE_WIDTH ? v : 760;
}
const ideWidth = ref(loadIdeWidth());
let dragging = false;

function onDrag(e: MouseEvent) {
  if (!dragging) return;
  // Width is measured from the right edge — drag left to widen the editor.
  const fromRight = window.innerWidth - e.clientX;
  const max = Math.max(MIN_IDE_WIDTH, window.innerWidth - MIN_IDE_WIDTH);
  ideWidth.value = Math.min(Math.max(fromRight, MIN_IDE_WIDTH), max);
}
function stopDrag() {
  if (!dragging) return;
  dragging = false;
  document.removeEventListener('mousemove', onDrag);
  document.removeEventListener('mouseup', stopDrag);
  document.body.style.userSelect = '';
  localStorage.setItem('loom.ideWidth', String(Math.round(ideWidth.value)));
}
function startDrag(e: MouseEvent) {
  dragging = true;
  e.preventDefault();
  document.addEventListener('mousemove', onDrag);
  document.addEventListener('mouseup', stopDrag);
  // Suppress text selection while dragging the divider.
  document.body.style.userSelect = 'none';
}

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
  // Gate the editor affordance on the server setting (cheap; the panel itself
  // re-checks availability when opened).
  ideInfo(props.id)
    .then((info) => (ideEnabled.value = info.enabled))
    // Best-effort: if the probe fails the editor affordance just stays hidden,
    // which is the safe default — nothing else on the page depends on it.
    .catch(() => {});
});
onUnmounted(() => {
  source?.close();
  stopDrag();
});
</script>

<template>
  <!-- A horizontal split fills the workbench main area: the session page (header
       + tabs + terminal/overview) on the left, and the embedded editor pulled in
       from the right. The editor is a resizable, collapsible panel — closed by
       default so a session-open never spawns a code-server. -->
  <div v-if="ws" class="flex min-h-[28rem] flex-1">
    <!-- Left: the session page. min-w-0 lets it shrink as the editor widens;
         AgentTerminal's ResizeObserver re-fits the terminal on the change. -->
    <div class="flex min-w-0 flex-1 flex-col px-5 py-3">
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
        <!-- Terminal (default) — the working zone: the live agent, plus on-demand
             worktree debug shells in an inner tab strip. v-show, NEVER v-if:
             keeping the agent terminal's host in the DOM means its zero-size
             guard skips the bogus resize while hidden and its ResizeObserver
             re-fits on return. -->
        <section v-show="tab === 'terminal'" class="h-full">
          <SessionTerminals :id="props.id" />
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

        <!-- Conversation — the agent's chat with the model (live, or the
             archived capture). Mounted only when selected; it fetches on its
             own and re-fetches via its Refresh button. -->
        <div v-if="tab === 'conversation'" class="h-full">
          <SessionConversation :id="props.id" />
        </div>
      </div>
    </div>

    <!-- Editor side panel (only when enabled in settings). -->
    <template v-if="ideEnabled">
      <!-- Open: a draggable divider + the editor at the persisted width. -->
      <template v-if="ideOpen">
        <div
          class="w-1 shrink-0 cursor-col-resize bg-line hover:bg-accent"
          title="Drag to resize the editor"
          @mousedown="startDrag"
        ></div>
        <section
          class="relative flex shrink-0 flex-col border-l border-line"
          :style="{ width: ideWidth + 'px' }"
        >
          <button
            class="absolute right-1 top-1 z-10 rounded px-1.5 py-0.5 text-xs text-muted hover:bg-subtle hover:text-fg"
            title="Close editor"
            aria-label="Close editor"
            @click="ideOpen = false"
          >
            ✕
          </button>
          <IdeFrame :id="props.id" :work-dir="ws.work_dir" class="min-h-0 flex-1" />
        </section>
      </template>

      <!-- Closed: a thin edge handle to pull the editor in from the right. -->
      <button
        v-else
        class="group flex shrink-0 items-center border-l border-line bg-surface px-1 text-muted hover:bg-subtle hover:text-fg"
        title="Open the editor"
        data-testid="ide-open"
        @click="ideOpen = true"
      >
        <span class="[writing-mode:vertical-rl] rotate-180 py-2 text-xs font-medium tracking-wide"
          >‹ Editor</span
        >
      </button>
    </template>
  </div>
  <p v-else class="px-5 py-3 text-sm text-muted">Loading…</p>
</template>
