<script setup lang="ts">
import {
  ref,
  reactive,
  computed,
  watch,
  onMounted,
  onActivated,
  onDeactivated,
  onUnmounted,
  nextTick,
} from 'vue';
import { onBeforeRouteLeave, onBeforeRouteUpdate, useRoute, useRouter } from 'vue-router';
import { clearSessionTag, get, getSession, ideInfo, markChannelRead } from '../api';
import type { Session, WeaverEvent } from '../types';
import SessionTerminals from '../components/SessionTerminals.vue';
import IdeFrame from '../components/IdeFrame.vue';
import ScratchPanel from '../components/ScratchPanel.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import SessionTabs from '../components/SessionTabs.vue';
import SessionConversation from '../components/SessionConversation.vue';
import ArtifactsPanel from '../components/ArtifactsPanel.vue';
import ChangesPanel from '../components/ChangesPanel.vue';
import { cancelSessionBacktrack, completeSessionOpen } from '../lib/workbenchMetrics';
import { openSessionEvents, type SessionEventsHandle } from '../lib/sessionEvents';
import { useCommandScope, type Command } from '../lib/commands';
import { signalChips } from '../lib/sessionState';

// Named + keyed-by-id in App.vue's <keep-alive> so the page (and its live
// terminal) stays warm: every `/s/:id…` path (the work tabs and the Artifacts
// deep-links) resolves to this one instance, so moving terminal ⇄ artifacts is a
// tab flip on a warm page — no remount, no reconnect, no jump.
defineOptions({ name: 'SessionDetail' });

const props = defineProps<{ id: string; name?: string }>();
const route = useRoute();
const router = useRouter();

// The fleet cache is intentionally compact. A session page fetches the complete
// resource on demand rather than pretending its summary is full detail.
const ws = ref<Session | null>(null);
const events = ref<WeaverEvent[]>([]);
const error = ref('');

// --- Work-area tabs --------------------------------------------------------
// The local panes the parent flips under v-show (never v-if for a live terminal
// — tearing down the WebSocket/xterm is the worst thing on a terminal-first
// page). Artifacts is route-backed (`/s/:id/artifacts` is this same component) so
// it stays deep-linkable and refresh-stable, and its heavy viewer lazily mounts
// only once opened.
//
// The set + order depend on the backend: a terminal session leads with Terminal
// (the live agent's TUI); an ACP session is headless, so it leads with
// Conversation and demotes the worktree shells to a slim Shells tab. `defaultTab`
// resolves whichever leads when the user hasn't picked one.
type LocalTab = 'terminal' | 'conversation' | 'shells';
type WorkTab = LocalTab | 'review';
const isAcp = computed(() => ws.value?.protocol === 'acp');
const defaultTab = computed<LocalTab>(() => (isAcp.value ? 'conversation' : 'terminal'));

const VALID_LOCAL = ['terminal', 'conversation', 'shells'];
const initialTab = route.query.tab;
// `null` means "follow the backend's default tab"; a real value is an explicit
// pick (from the URL or a click) that sticks.
const localTab = ref<LocalTab | null>(
  typeof initialTab === 'string' && VALID_LOCAL.includes(initialTab)
    ? (initialTab as LocalTab)
    : null,
);
const effectiveLocalTab = computed<LocalTab>(() => localTab.value ?? defaultTab.value);

// The artifacts surface is open whenever the path is under `…/artifacts`.
const artifactsActive = computed(() => route.path.startsWith(`/s/${props.id}/artifacts`));
const changesActive = computed(() => route.path === `/s/${props.id}/changes`);
const reviewActive = computed(() => artifactsActive.value || changesActive.value);

// Popped out into the rail beside the work area vs docked as the work-area tab.
// Transient (defaults docked on a fresh open); only the rail *width* persists.
const poppedOut = ref(false);
const artifactsDocked = computed(() => artifactsActive.value && !poppedOut.value);
const reviewDocked = computed(() => artifactsDocked.value || changesActive.value);
const railOpen = computed(() => artifactsActive.value && poppedOut.value);
const dockedArtifactsRef = ref<InstanceType<typeof ArtifactsPanel> | null>(null);
const railArtifactsRef = ref<InstanceType<typeof ArtifactsPanel> | null>(null);
const acpShellsRef = ref<InstanceType<typeof SessionTerminals> | null>(null);
let layoutNavigationAuthorized = false;

function activeArtifactsPanel(): InstanceType<typeof ArtifactsPanel> | null {
  return railOpen.value ? railArtifactsRef.value : dockedArtifactsRef.value;
}

// The pane the work area shows: the artifacts panel when docked, else the
// effective local tab (so a popped-out artifact leaves the work pane in place).
const workTab = computed<WorkTab>(() => (reviewDocked.value ? 'review' : effectiveLocalTab.value));

// Lazy-mount panes on first visit, then keep them (v-show) so re-selecting is
// instant. The terminal is always mounted; the rest start cold so a session-open
// stays cheap. Watch the pane actually on screen, not the backend's default:
// an ACP artifact deep-link is docked over its default Conversation tab and must
// not fetch/render a potentially huge chat behind the requested document.
const mounted = reactive({
  conversation: false,
  shells: false,
  artifacts: artifactsActive.value,
  changes: changesActive.value,
});
watch(
  workTab,
  (t) => {
    if (t === 'conversation' || t === 'shells') mounted[t] = true;
  },
  { immediate: true },
);
watch(
  artifactsActive,
  (on) => {
    if (on) mounted.artifacts = true;
  },
  { immediate: true },
);
watch(
  changesActive,
  (on) => {
    if (on) mounted.changes = true;
  },
  { immediate: true },
);

async function guardedArtifactLayout(change: () => void | Promise<void>): Promise<boolean> {
  const panel = activeArtifactsPanel();
  if (panel && !(await panel.prepareLayoutSwap())) return false;
  try {
    layoutNavigationAuthorized = true;
    await change();
    await nextTick();
    return true;
  } finally {
    layoutNavigationAuthorized = false;
    panel?.finishLayoutSwap();
  }
}

async function selectTab(t: WorkTab) {
  if (t === 'review') {
    // Review owns the deep-linked Artifacts / Changes choice. Reopening it
    // preserves the current choice and otherwise defaults to Artifacts.
    await guardedArtifactLayout(async () => {
      poppedOut.value = false;
      if (!reviewActive.value) await router.push(`/s/${props.id}/artifacts`);
    });
    return;
  }
  if (t === 'conversation' || t === 'shells') mounted[t] = true;
  localTab.value = t;
  // Leaving a docked artifacts surface for a local tab closes it (back to the
  // plain session URL); when it's popped out the rail stays and we just swap the
  // work-area pane.
  if (reviewDocked.value) {
    await guardedArtifactLayout(async () => {
      await router.push(`/s/${props.id}`);
    });
  }
}

const workTabs = computed<{ key: WorkTab; label: string }[]>(() =>
  isAcp.value
    ? [
        { key: 'conversation', label: 'Conversation' },
        { key: 'shells', label: 'Shells' },
        { key: 'review', label: 'Review' },
      ]
    : [
        { key: 'terminal', label: 'Agent' },
        { key: 'conversation', label: 'Conversation' },
        { key: 'review', label: 'Review' },
      ],
);
async function moveWorkTab(direction: -1 | 1) {
  const tabs = workTabs.value;
  const current = tabs.findIndex((tab) => tab.key === workTab.value);
  const next = (Math.max(current, 0) + direction + tabs.length) % tabs.length;
  await selectTab(tabs[next].key);
}
const sessionCommands = computed<Command[]>(() => [
  {
    id: 'session.back',
    label: 'Back to sessions',
    keys: ['b', 'Escape'],
    hint: true,
    run: () => void router.push('/'),
  },
  ...workTabs.value.map((tab, index) => ({
    id: `session.tab.${tab.key}`,
    label: `Open ${tab.label}`,
    keys: [String(index + 1)],
    run: () => selectTab(tab.key),
  })),
  {
    id: 'session.tab.previous',
    label: 'Previous work surface',
    keys: ['['],
    run: () => moveWorkTab(-1),
  },
  {
    id: 'session.tab.next',
    label: 'Next work surface',
    keys: [']'],
    hint: true,
    run: () => moveWorkTab(1),
  },
  ...(reviewActive.value
    ? [
        {
          id: 'session.review.artifacts',
          label: 'Open Artifacts',
          keys: ['a'],
          run: async () => {
            await guardedArtifactLayout(async () => {
              await router.push(`/s/${props.id}/artifacts`);
            });
          },
        },
        {
          id: 'session.review.changes',
          label: 'Open Changes',
          keys: ['c'],
          run: async () => {
            await guardedArtifactLayout(async () => {
              await router.push(`/s/${props.id}/changes`);
            });
          },
        },
      ]
    : []),
  ...(isAcp.value && workTab.value === 'shells'
    ? [
        {
          id: 'session.shell.new',
          label: 'Open a worktree shell',
          keys: ['n'],
          hint: true,
          run: () => acpShellsRef.value?.addShell(),
        },
      ]
    : []),
]);
useCommandScope(`session:${props.id}`, 'Session', sessionCommands, 10);

// Pop the artifact out beside the terminal / dock it back into the tab.
async function togglePop() {
  await guardedArtifactLayout(() => {
    poppedOut.value = !poppedOut.value;
  });
}
// Close the rail entirely — back to the plain session page.
async function closeRail() {
  await guardedArtifactLayout(async () => {
    poppedOut.value = false;
    await router.push(`/s/${props.id}`);
  });
}

// --- Resizable side rails --------------------------------------------------
// Two on-demand panels pull in from the right: the artifact (popped out) and the
// embedded editor. Each persists its own width and drags from the right edge.
const MIN_PANEL_WIDTH = 360;
function loadWidth(key: string, fallback: number): number {
  const v = Number(localStorage.getItem(key));
  return Number.isFinite(v) && v >= MIN_PANEL_WIDTH ? v : fallback;
}
const artifactWidth = ref(loadWidth('loom.artifactWidth', 620));
const ideWidth = ref(loadWidth('loom.ideWidth', 760));
function panelWidth(width: number): { width: string } {
  // On narrow windows the app rail still needs room; saved desktop widths must
  // not push the close control or document scroller off-screen.
  return { width: `min(${width}px, calc(100vw - 3.5rem))` };
}

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

let source: SessionEventsHandle | null = null;
let sessionLoadEpoch = 0;

async function loadSession() {
  const epoch = ++sessionLoadEpoch;
  const session = await getSession(props.id);
  if (epoch === sessionLoadEpoch) ws.value = session;
}

async function acknowledgeSessionAttention(): Promise<string> {
  const epoch = ++sessionLoadEpoch;
  let session = await getSession(props.id);
  try {
    // The session id is also its default channel id. Opening the workbench is
    // the user's acknowledgement gesture for both legacy tags and channel
    // urgency; future messages raise unread attention again.
    await markChannelRead(props.id);
  } catch {
    // A read receipt is best-effort bookkeeping, not a failed session load.
    // Leave the channel unread so another view can retry without presenting an
    // internal acknowledgement failure the user cannot act on.
  }
  // Attention is a wake-up signal, not a permanent task state. Entering (or
  // returning to) a session acknowledges every loud tag visible in the
  // snapshot the user opened. Lifecycle failures remain visible because they
  // are derived state rather than dismissible tags; a later tag write raises
  // attention again through the normal event stream.
  const keys = [...new Set(signalChips(session).map((chip) => chip.key))];
  if (!keys.length) {
    if (epoch === sessionLoadEpoch) ws.value = session;
    return '';
  }
  const results = await Promise.allSettled(keys.map((key) => clearSessionTag(props.id, key)));
  // Re-read even after a partial failure: successful acknowledgements should
  // disappear, while any failed tag remains available for the existing manual
  // clear gesture.
  session = await getSession(props.id);
  if (epoch === sessionLoadEpoch) ws.value = session;
  const failed = results.filter((result) => result.status === 'rejected').length;
  const notices = [];
  if (failed) {
    notices.push(`Couldn't acknowledge ${failed} attention signal${failed === 1 ? '' : 's'}.`);
  }
  return notices.join(' ');
}

async function loadAllWith(sessionLoad: () => Promise<string>) {
  try {
    const acknowledgementError = await sessionLoad();
    events.value = (await get(`/sessions/${props.id}/log`)) as WeaverEvent[];
    error.value = acknowledgementError;
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function loadAll() {
  return loadAllWith(async () => {
    await loadSession();
    return '';
  });
}

async function acknowledgeAndLoadAll() {
  return loadAllWith(acknowledgeSessionAttention);
}

function closeStream() {
  source?.close();
  source = null;
}

function openStream() {
  closeStream();
  source = openSessionEvents(props.id);
  // `tag` covers every status axis (the agent's attention, a watch's
  // triage, any free-form key); a tag write re-fetches the session so the
  // resolved badge and the pill row refresh.
  for (const kind of ['status', 'tag', 'github', 'handoff', 'metadata']) {
    source.on(kind, (e) => {
      const ev = JSON.parse(e.data) as WeaverEvent;
      events.value.push(ev);
      loadSession().catch(() => {});
    });
  }
  // The Issues deep link carries the branch's live open count. Refresh the
  // session projection when the ledger changes without restoring the deleted
  // issue/overview pane or duplicating issue state in this view.
  for (const kind of ['issue_added', 'issue_closed', 'issue_reopened']) {
    source.on(kind, () => loadSession().catch(() => {}));
  }
}

onMounted(() => {
  requestAnimationFrame(() => completeSessionOpen(props.id));
  acknowledgeAndLoadAll();
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
  requestAnimationFrame(() => completeSessionOpen(props.id));
  acknowledgeAndLoadAll();
  openStream();
});

function finishArtifactSwapAfterNavigation(panel: InstanceType<typeof ArtifactsPanel>) {
  const removeAfterEach = router.afterEach(() => {
    removeAfterEach();
    panel.finishLayoutSwap();
  });
}

onBeforeRouteUpdate(async (to, from) => {
  const nextName = typeof to.params.name === 'string' ? to.params.name : '';
  const previousName = typeof from.params.name === 'string' ? from.params.name : '';
  const changesArtifact =
    to.params.id === props.id &&
    from.params.id === props.id &&
    to.path.startsWith(`/s/${props.id}/artifacts`) &&
    from.path.startsWith(`/s/${props.id}/artifacts`) &&
    nextName !== previousName;
  if (!changesArtifact) return;
  const panel = activeArtifactsPanel();
  if (!panel || panel.isOpeningArtifact(nextName)) return;
  if (!(await panel.prepareLayoutSwap())) return false;
  finishArtifactSwapAfterNavigation(panel);
});

onBeforeRouteLeave(async (to) => {
  const staysInArtifacts =
    to.params.id === props.id && to.path.startsWith(`/s/${props.id}/artifacts`);
  if (artifactsActive.value && !staysInArtifacts && !layoutNavigationAuthorized) {
    const panel = activeArtifactsPanel();
    if (panel && !(await panel.prepareLayoutSwap())) return false;
    if (panel) {
      // A direct rail link or browser-history navigation has no local layout
      // callback. Hold the frozen controller through the router commit (or
      // abort), then release the still-mounted cached surface.
      finishArtifactSwapAfterNavigation(panel);
    }
  }
  const staysInSession = to.params.id === props.id && to.path.startsWith(`/s/${props.id}`);
  if (to.path !== '/' && !staysInSession) cancelSessionBacktrack();
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
  <div v-if="ws" class="flex min-h-0 flex-1 overflow-hidden">
    <!-- Left: the session page. min-w-0 lets it shrink as panels widen;
         AgentTerminal's ResizeObserver re-fits the terminal on the change. -->
    <div class="flex min-h-0 min-w-0 flex-1 flex-col px-3 py-2 sm:px-5 sm:py-3">
      <SessionPageHeader
        :ws="ws"
        :events="events"
        :ide-enabled="ideEnabled"
        @reload="loadAll"
        @open-editor="ideOpen = true"
      />
      <SessionTabs
        :tab="workTab"
        :artifacts-popped="railOpen"
        :protocol="ws.protocol"
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
        <!-- Terminal (terminal sessions) — the working zone: the live agent, plus
             on-demand worktree debug shells in an inner tab strip. v-show, NEVER
             v-if. An ACP session is headless, so it has no Terminal pane. -->
        <section v-if="!isAcp" v-show="workTab === 'terminal'" class="h-full">
          <SessionTerminals :id="props.id" />
        </section>

        <!-- Shells (ACP sessions) — the worktree escape hatch: the same terminal
             area with the Agent inner tab dropped. Lazily mounted on first open,
             then kept (v-show) so re-selecting is instant. -->
        <div v-if="isAcp && mounted.shells" v-show="workTab === 'shells'" class="h-full">
          <SessionTerminals ref="acpShellsRef" :id="props.id" shells-only />
        </div>

        <!-- Conversation — the agent's chat with the model. Lazily mounted, then
             kept (v-show) so flipping back is instant. -->
        <div v-if="mounted.conversation" v-show="workTab === 'conversation'" class="h-full">
          <SessionConversation :session="ws" />
        </div>

        <!-- Review is the one route owner for the kept-alive Artifacts and
             Changes renderers. Existing artifact deep links stay canonical. -->
        <div
          v-if="(mounted.artifacts || mounted.changes) && !railOpen"
          v-show="reviewDocked"
          class="flex h-full min-h-0 flex-col"
        >
          <nav class="flex shrink-0 gap-1 border-b border-line px-2 text-xs" aria-label="Review">
            <router-link
              :to="`/s/${props.id}/artifacts`"
              class="border-b-2 px-2 py-1.5"
              :class="artifactsActive ? 'border-accent text-fg' : 'border-transparent text-muted'"
            >
              Artifacts
            </router-link>
            <router-link
              :to="`/s/${props.id}/changes`"
              class="border-b-2 px-2 py-1.5"
              :class="changesActive ? 'border-accent text-fg' : 'border-transparent text-muted'"
            >
              Changes
            </router-link>
          </nav>
          <div v-if="mounted.artifacts" v-show="artifactsDocked" class="min-h-0 flex-1">
            <ArtifactsPanel
              ref="dockedArtifactsRef"
              :id="props.id"
              :name="props.name"
              :active="artifactsActive"
              @toggle-pop="togglePop"
            />
          </div>
          <div v-if="mounted.changes" v-show="changesActive" class="min-h-0 flex-1">
            <ChangesPanel :id="props.id" />
          </div>
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
        class="flex min-h-0 shrink-0 flex-col overflow-hidden border-l border-line"
        :style="panelWidth(artifactWidth)"
      >
        <ArtifactsPanel
          ref="railArtifactsRef"
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
          class="relative flex min-h-0 shrink-0 flex-col overflow-hidden border-l border-line"
          :style="panelWidth(ideWidth)"
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
    </template>
  </div>
  <!-- The session never loaded (missing id, or the fetch failed). `error` is the
       only signal we have — without this branch the page sits on "Loading…"
       forever, because the in-page error line lives inside the `ws` subtree. -->
  <p v-else-if="error" class="px-5 py-3 text-sm text-block">{{ error }}</p>
  <p v-else class="px-5 py-3 text-sm text-muted">Loading…</p>
</template>
