<script setup lang="ts">
import { ref, reactive, computed, watch, onMounted, onActivated, onDeactivated, onUnmounted } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { get, ideInfo } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import SessionTerminals from '../components/SessionTerminals.vue';
import IdeFrame from '../components/IdeFrame.vue';
import ScratchPanel from '../components/ScratchPanel.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import SessionTabs from '../components/SessionTabs.vue';
import SessionOverview from '../components/SessionOverview.vue';
import SessionConversation from '../components/SessionConversation.vue';
import ArtifactsPanel from '../components/ArtifactsPanel.vue';
import { useFleet } from '../lib/sessionsStore';

// Named + keyed-by-id in App.vue's <keep-alive> so the page (and its live
// terminal) stays warm: every `/s/:id…` path (the work tabs and the Artifacts
// deep-links) resolves to this one instance, so moving terminal ⇄ artifacts is a
// tab flip on a warm page — no remount, no reconnect, no jump.
defineOptions({ name: 'SessionDetail' });

const props = defineProps<{ id: string; name?: string }>();
const route = useRoute();
const router = useRouter();

// Seed from the shared fleet snapshot so the page paints immediately with the
// row the list already had — no "Loading…" gap while the per-session refetch is
// in flight. loadAll() still refreshes it to the full per-session view.
const { sessionById } = useFleet();
const ws = ref<Session | null>(sessionById(props.id) ?? null);
const events = ref<WeaverEvent[]>([]);
const issues = ref<Issue[]>([]);
const backlog = ref<Issue[]>([]);
const error = ref('');

// --- Work-area tabs --------------------------------------------------------
// Terminal/Overview/Conversation are local panes the parent flips under v-show
// (never v-if for the terminal — tearing down the WebSocket/xterm is the worst
// thing on a terminal-first page). Artifacts is route-backed (`/s/:id/artifacts`
// is this same component) so it stays deep-linkable and refresh-stable, and its
// heavy viewer lazily mounts only once opened.
type LocalTab = 'terminal' | 'overview' | 'conversation';
type WorkTab = LocalTab | 'artifacts';
const initialTab = route.query.tab;
const localTab = ref<LocalTab>(
  initialTab === 'overview' || initialTab === 'conversation' ? initialTab : 'terminal',
);

// The artifacts surface is open whenever the path is under `…/artifacts`.
const artifactsActive = computed(() => route.path.startsWith(`/s/${props.id}/artifacts`));

// Popped out into the rail beside the terminal vs docked as the work-area tab.
// Transient (defaults docked on a fresh open); only the rail *width* persists.
const poppedOut = ref(false);
const artifactsDocked = computed(() => artifactsActive.value && !poppedOut.value);
const railOpen = computed(() => artifactsActive.value && poppedOut.value);

// The pane the work area shows: the artifacts panel when docked, else the last
// local tab (so a popped-out artifact leaves the terminal in the work area).
const workTab = computed<WorkTab>(() => (artifactsDocked.value ? 'artifacts' : localTab.value));

// Lazy-mount panes on first visit, then keep them (v-show) so re-selecting is
// instant. The terminal is always mounted; the rest start cold so a session-open
// stays cheap (the artifact viewer only loads once its tab is opened).
const mounted = reactive({
  overview: localTab.value === 'overview',
  conversation: localTab.value === 'conversation',
  artifacts: artifactsActive.value,
});
watch(
  artifactsActive,
  (on) => {
    if (on) mounted.artifacts = true;
  },
  { immediate: true },
);

function selectTab(t: WorkTab) {
  if (t === 'artifacts') {
    // The Artifacts tab is the docked view: bring the surface into the work
    // area (docking it if it was popped out), opening it if it was closed.
    poppedOut.value = false;
    if (!artifactsActive.value) router.push(`/s/${props.id}/artifacts`);
    return;
  }
  if (t === 'overview' || t === 'conversation') mounted[t] = true;
  localTab.value = t;
  // Leaving a docked artifacts surface for a local tab closes it (back to the
  // plain session URL); when it's popped out the rail stays and we just swap the
  // work-area pane.
  if (artifactsDocked.value) router.push(`/s/${props.id}`);
}

// Pop the artifact out beside the terminal / dock it back into the tab.
function togglePop() {
  poppedOut.value = !poppedOut.value;
}
// Close the rail entirely — back to the plain session page.
function closeRail() {
  poppedOut.value = false;
  router.push(`/s/${props.id}`);
}

const issueCount = computed(() => issues.value.length + backlog.value.length);

// --- Resizable side rails --------------------------------------------------
// Two panels pull in from the right: the artifact (popped out) and the embedded
// editor. Each persists its own width and drags from the right edge.
const MIN_PANEL_WIDTH = 360;
function loadWidth(key: string, fallback: number): number {
  const v = Number(localStorage.getItem(key));
  return Number.isFinite(v) && v >= MIN_PANEL_WIDTH ? v : fallback;
}
const artifactWidth = ref(loadWidth('loom.artifactWidth', 620));
const ideWidth = ref(loadWidth('loom.ideWidth', 760));

// Each rail drags from the right edge and persists its own width; a single
// discriminator picks which one a divider drives (templates auto-unwrap refs, so
// the rail is named, not passed by reference).
type Rail = 'artifact' | 'ide';
const RAILS: Record<Rail, { width: typeof artifactWidth; key: string }> = {
  artifact: { width: artifactWidth, key: 'loom.artifactWidth' },
  ide: { width: ideWidth, key: 'loom.ideWidth' },
};
let dragging: Rail | null = null;
function onDrag(e: MouseEvent) {
  if (!dragging) return;
  // Width is measured from the right edge — drag left to widen the panel.
  const fromRight = window.innerWidth - e.clientX;
  const max = Math.max(MIN_PANEL_WIDTH, window.innerWidth - MIN_PANEL_WIDTH);
  RAILS[dragging].width.value = Math.min(Math.max(fromRight, MIN_PANEL_WIDTH), max);
}
function stopDrag() {
  if (!dragging) return;
  const rail = RAILS[dragging];
  localStorage.setItem(rail.key, String(Math.round(rail.width.value)));
  dragging = null;
  document.removeEventListener('mousemove', onDrag);
  document.removeEventListener('mouseup', stopDrag);
  document.body.style.userSelect = '';
}
function startDrag(which: Rail, e: MouseEvent) {
  dragging = which;
  e.preventDefault();
  document.addEventListener('mousemove', onDrag);
  document.addEventListener('mouseup', stopDrag);
  // Suppress text selection while dragging the divider.
  document.body.style.userSelect = 'none';
}

// --- Embedded editor (code-server) side panel ------------------------------
// The editor lives in a resizable panel pulled in from the right, beside the
// live terminal. Closed by default and mounted only when open, so opening it is
// what lazily spawns the session's code-server. `ideEnabled` gates the whole
// affordance on the server setting.
const ideEnabled = ref(false);
const ideOpen = ref(false);

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

function closeStream() {
  source?.close();
  source = null;
}

function openStream() {
  closeStream();
  source = new EventSource(`/api/sessions/${props.id}/events`);
  // `tag` covers every status axis (the agent's attention, a watch's
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
  // Overview's pinned-plan watcher to re-fetch the `plan` artifact. The
  // Artifacts panel refreshes off its own SSE subscription.
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
// The events SSE is paused while the page is off-screen (kept alive). A cached
// SessionDetail would otherwise hold an EventSource open while parked on another
// session — idle streams stacking up against the browser's per-origin HTTP/1.1
// connection cap. The terminal WebSocket (a separate pool) stays warm
// regardless. onMounted owns the first open; onActivated reopens + refetches on
// a *return* (guarded by `source` so the initial mount never double-opens).
onActivated(() => {
  if (source) return; // initial mount already loaded + opened the stream
  loadAll();
  openStream();
});
onDeactivated(closeStream);
onUnmounted(() => {
  closeStream();
  stopDrag();
});
</script>

<template>
  <!-- A horizontal split fills the workbench main area: the session page (header
       + tabs + work area) on the left, then any panels pulled in from the right
       — the popped-out artifact and the embedded editor, each resizable. -->
  <div v-if="ws" class="flex min-h-[28rem] flex-1">
    <!-- Left: the session page. min-w-0 lets it shrink as panels widen;
         AgentTerminal's ResizeObserver re-fits the terminal on the change. -->
    <div class="flex min-w-0 flex-1 flex-col px-5 py-3">
      <SessionPageHeader :ws="ws" @reload="loadAll" />
      <SessionTabs
        :tab="workTab"
        :id="props.id"
        :issue-count="issueCount"
        :artifacts-popped="railOpen"
        @select="selectTab"
      >
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
             worktree debug shells in an inner tab strip. v-show, NEVER v-if. -->
        <section v-show="workTab === 'terminal'" class="h-full">
          <SessionTerminals :id="props.id" />
        </section>

        <!-- Overview — read-only context (goal, issues, activity). Mounted on
             first visit, then kept (v-show) so re-selecting it is instant. -->
        <div
          v-if="mounted.overview"
          v-show="workTab === 'overview'"
          class="h-full overflow-auto pb-1"
        >
          <SessionOverview
            :ws="ws"
            :events="events"
            :format="eventLine"
            :issues="issues"
            :backlog="backlog"
          />
        </div>

        <!-- Conversation — the agent's chat with the model. Lazily mounted, then
             kept (v-show) so flipping back is instant. -->
        <div v-if="mounted.conversation" v-show="workTab === 'conversation'" class="h-full">
          <SessionConversation :session="ws" />
        </div>

        <!-- Artifacts (docked) — fills the work area as a tab. Lazily mounted on
             first open, then kept (hidden via v-show) so flipping terminal ⇄
             artifacts is instant. Unmounts only when popped out, where the rail
             copy below takes over. -->
        <div v-if="mounted.artifacts && !railOpen" v-show="artifactsDocked" class="h-full">
          <ArtifactsPanel
            :id="props.id"
            :name="props.name"
            :active="artifactsActive"
            @toggle-pop="togglePop"
          />
        </div>
      </div>
    </div>

    <!-- Artifact rail (popped out): a draggable divider + the panel at its
         persisted width, beside the terminal. A second, compact mount of the
         same view — opening it restores the artifact from the URL, so the docked
         tab can stay warm for the instant terminal ⇄ artifacts flip. -->
    <template v-if="railOpen">
      <div
        class="w-1 shrink-0 cursor-col-resize bg-line hover:bg-accent"
        title="Drag to resize the artifact panel"
        @mousedown="(e) => startDrag('artifact', e)"
      ></div>
      <section
        class="flex shrink-0 flex-col border-l border-line"
        :style="{ width: artifactWidth + 'px' }"
      >
        <ArtifactsPanel
          :id="props.id"
          :name="props.name"
          :active="railOpen"
          compact
          popped
          class="min-h-0 flex-1"
          @toggle-pop="togglePop"
          @close="closeRail"
        />
      </section>
    </template>

    <!-- Editor side panel (only when enabled in settings). -->
    <template v-if="ideEnabled">
      <!-- Open: a draggable divider + the editor at the persisted width. -->
      <template v-if="ideOpen">
        <div
          class="w-1 shrink-0 cursor-col-resize bg-line hover:bg-accent"
          title="Drag to resize the editor"
          @mousedown="(e) => startDrag('ide', e)"
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
