<script setup lang="ts">
import {
  ref,
  reactive,
  computed,
  onMounted,
  onActivated,
  onDeactivated,
  onUnmounted,
  watch,
  nextTick,
} from 'vue';
import {
  getSessionChat,
  promptSession,
  interruptSession,
  answerPermission,
  setSessionMode,
} from '../api';
import type {
  Session,
  ChatBlock,
  SseDelta,
  SseTool,
  SseTurn,
  UserMessagePayload,
  AgentMessagePayload,
  ThoughtPayload,
  ToolCallPayload,
  ToolContent,
  PlanEntry,
  PlanPayload,
  PermissionPayload,
  UsagePayload,
  TurnEndPayload,
} from '../types';
import { canSend } from '../lib/sessionState';
import MarkdownView from './MarkdownView.vue';

// The Conversation surface for an *ACP* session (`protocol='acp'`): typeset
// dialogue, not chat bubbles. Serif prose for the humans and the agent, the
// machine's apparatus (tool calls, diffs, command output) set as indented mono
// blocks between the prose — a scholarly edition, footnotes apart from text.
//
// Its data source is the durable chat journal: it paints the `GET /chat`
// snapshot, then applies the `/chat/stream` SSE tail in place — `block` upserts
// by (turn, seq), `delta` streams into a shadow message/thought, `tool` tracks
// live tool state, `turn` drives the working indicator. The composer posts to
// `/prompt`; Stop posts `/interrupt`; permission cards answer via
// `/permissions/{id}`; the mode chip drives `/mode`.
const props = defineProps<{ session: Session }>();
const id = computed(() => props.session.id);

// ── The render state the stream feeds ────────────────────────────────────────
const blocks = reactive(new Map<string, ChatBlock>());
const shadows = reactive(
  new Map<string, { turn: number; kind: 'agent_message' | 'thought'; text: string }>(),
);
const liveTools = reactive(new Map<string, SseTool>());
const turnLive = ref(false);
const liveTurnNo = ref<number | null>(null);
const optimistic = ref<{ text: string; queued: boolean }[]>([]);

// The live mode, seeded from the session and advanced by `mode_change` blocks or
// a local set — so the composer chip reads true without a refetch.
const currentMode = ref<string | null>(props.session.current_mode);
watch(
  () => props.session.current_mode,
  (m) => {
    if (m) currentMode.value = m;
  },
);

const blockKey = (turn: number, seq: number) => `${turn}:${seq}`;

type LoadState = 'loading' | 'ready' | 'error';
const state = ref<LoadState>('loading');
const errorMsg = ref('');

let loadSeq = 0;
async function load({ preserve = false }: { preserve?: boolean } = {}) {
  const seq = ++loadSeq;
  if (!preserve) state.value = 'loading';
  const stick = preserve && nearBottom();
  try {
    const snap = await getSessionChat(id.value);
    if (seq !== loadSeq) return;
    blocks.clear();
    for (const b of snap.blocks) blocks.set(blockKey(b.turn, b.seq), b);
    shadows.clear();
    liveTools.clear();
    liveTurnNo.value = snap.live_turn;
    turnLive.value = snap.live_turn != null;
    state.value = 'ready';
  } catch (e) {
    if (seq !== loadSeq) return;
    if (!preserve) {
      errorMsg.value = (e as Error).message ?? 'Failed to load conversation';
      state.value = 'error';
    }
    return;
  }
  await nextTick();
  if (seq !== loadSeq) return;
  if (stick || !preserve) scrollToBottom();
  updateActive();
}

// ── Stream application ───────────────────────────────────────────────────────
function onBlock(b: ChatBlock) {
  blocks.set(blockKey(b.turn, b.seq), b);
  if (b.kind === 'tool_call') {
    const tid = (b.payload as unknown as ToolCallPayload).tool_call_id;
    if (tid) liveTools.delete(tid);
  } else if (b.kind === 'agent_message' || b.kind === 'thought') {
    shadows.delete(`${b.turn}:${b.kind}`);
  } else if (b.kind === 'mode_change') {
    const m = (b.payload as { mode_id?: string }).mode_id;
    if (m) currentMode.value = m;
  } else if (b.kind === 'user_message') {
    const text = (b.payload as unknown as UserMessagePayload).text ?? '';
    const i = optimistic.value.findIndex((o) => o.text === text);
    if (i >= 0) optimistic.value.splice(i, 1);
  }
  autoFollow();
}

function onDelta(d: SseDelta) {
  turnLive.value = true;
  liveTurnNo.value = d.turn;
  const k = `${d.turn}:${d.kind}`;
  const cur = shadows.get(k) ?? { turn: d.turn, kind: d.kind, text: '' };
  cur.text += d.text;
  shadows.set(k, cur);
  autoFollow();
}

function onTool(t: SseTool) {
  liveTools.set(t.tool_call_id, t);
  turnLive.value = true;
  liveTurnNo.value = t.turn;
  autoFollow();
}

function onTurn(ev: SseTurn) {
  if (ev.state === 'started') {
    turnLive.value = true;
    liveTurnNo.value = ev.turn;
  } else {
    turnLive.value = false;
    liveTurnNo.value = null;
    optimistic.value = optimistic.value.filter((o) => o.queued);
  }
}

// ── SSE lifecycle (kept-alive discipline) ────────────────────────────────────
let source: EventSource | null = null;
function openStream() {
  source = new EventSource(`/api/sessions/${id.value}/chat/stream`);
  source.addEventListener('block', (e) => onBlock(JSON.parse((e as MessageEvent).data) as ChatBlock));
  source.addEventListener('delta', (e) => onDelta(JSON.parse((e as MessageEvent).data) as SseDelta));
  source.addEventListener('tool', (e) => onTool(JSON.parse((e as MessageEvent).data) as SseTool));
  source.addEventListener('turn', (e) => onTurn(JSON.parse((e as MessageEvent).data) as SseTurn));
}
function closeStream() {
  source?.close();
  source = null;
}

onMounted(() => {
  openStream();
  load();
});
onActivated(() => {
  if (source) return;
  openStream();
  load({ preserve: true });
});
onDeactivated(closeStream);
onUnmounted(closeStream);
watch(id, () => {
  closeStream();
  openStream();
  load();
});

// ── Composer ─────────────────────────────────────────────────────────────────
const draft = ref('');
const sending = ref(false);
const sendError = ref('');
const composerVisible = computed(() => canSend(props.session));

async function submitPrompt() {
  if (!draft.value.trim() || sending.value) return;
  sending.value = true;
  sendError.value = '';
  const text = draft.value;
  try {
    const ack = await promptSession(id.value, text);
    optimistic.value.push({ text, queued: ack.queued });
    draft.value = '';
    autoFollow();
  } catch (e) {
    sendError.value = (e as Error).message ?? 'Failed to send';
  } finally {
    sending.value = false;
  }
}

const stopping = ref(false);
async function stopTurn() {
  if (stopping.value) return;
  stopping.value = true;
  try {
    await interruptSession(id.value);
  } catch {
    /* the turn may have just ended; the SSE `turn` edge reconciles */
  } finally {
    stopping.value = false;
  }
}

// ── Mode chip ────────────────────────────────────────────────────────────────
// The well-known claude/codex ACP modes, used when the session doesn't expose an
// explicit `available_modes` list (SessionView carries only `current_mode`
// today). Wire the chip to `session.available_modes` the moment the server adds it.
const KNOWN_MODES = ['default', 'acceptEdits', 'plan', 'bypassPermissions'];
const MODE_LABEL: Record<string, string> = {
  default: 'default',
  acceptEdits: 'accept edits',
  plan: 'plan',
  bypassPermissions: 'bypass',
};
const modeOptions = computed(() => props.session.available_modes ?? KNOWN_MODES);
const modeLabel = (m: string | null) => (m ? (MODE_LABEL[m] ?? m) : 'mode');
const modeInteractive = computed(() => canSend(props.session) && modeOptions.value.length > 1);
const modeOpen = ref(false);
async function pickMode(m: string) {
  modeOpen.value = false;
  if (m === currentMode.value) return;
  const prev = currentMode.value;
  currentMode.value = m; // optimistic
  try {
    await setSessionMode(id.value, m);
  } catch {
    currentMode.value = prev; // the adapter refused it
  }
}
function onDocClick(e: MouseEvent) {
  if (modeOpen.value && !(e.target as HTMLElement).closest('[data-testid="acp-mode-chip"]')) {
    modeOpen.value = false;
  }
}
onMounted(() => document.addEventListener('click', onDocClick));
onUnmounted(() => document.removeEventListener('click', onDocClick));

// ── Permission answering ─────────────────────────────────────────────────────
const answering = ref<Set<string>>(new Set());
async function answer(perm: PermissionPayload, optionId: string) {
  if (answering.value.has(perm.request_id)) return;
  answering.value = new Set(answering.value).add(perm.request_id);
  try {
    await answerPermission(id.value, perm.request_id, optionId);
  } catch {
    /* the request may have been cancelled/resolved already */
  } finally {
    const s = new Set(answering.value);
    s.delete(perm.request_id);
    answering.value = s;
  }
}

// ── The render model ─────────────────────────────────────────────────────────
// Consecutive *quiet* tool calls (read/search/fetch/think/other, completed)
// collapse to one census line; consequential calls (edit/execute/delete/move, or
// failed, or still live) stand alone as cards.
const QUIET_KINDS = new Set(['read', 'search', 'fetch', 'think', 'other']);
function isQuiet(t: ToolCallPayload): boolean {
  return t.status === 'completed' && QUIET_KINDS.has(t.tool_kind || 'other');
}

type Row =
  | { type: 'turnRule'; key: string; turn: number; stop: string; ctx: number | null; loud: boolean }
  | { type: 'user'; key: string; anchor: string; n: number; time: string; text: string }
  | { type: 'agent'; key: string; time: string; text: string; streaming: boolean }
  | { type: 'thought'; key: string; text: string; streaming: boolean }
  | { type: 'census'; key: string; items: ToolCallPayload[] }
  | { type: 'card'; key: string; tool: ToolCallPayload; live: boolean }
  | { type: 'permission'; key: string; perm: PermissionPayload }
  | { type: 'mode'; key: string; mode: string };

interface TocItem {
  anchor: string;
  n: number;
  title: string;
}

function firstLine(s: string): string {
  return s.split('\n').map((l) => l.trim()).find(Boolean) ?? '';
}
function titleOf(text: string): string {
  const f = firstLine(text) || '(no text)';
  return f.replace(/^[#>\-*`\s]+/, '').trim() || f;
}
function shortTime(ts: string): string {
  return ts.length >= 16 && ts[10] === 'T' ? ts.slice(11, 16) : ts;
}

// The latest plan block feeds the right rail, not the transcript flow.
const latestPlan = computed<PlanEntry[]>(() => {
  let entries: PlanEntry[] = [];
  for (const b of blocks.values()) {
    if (b.kind === 'plan') entries = (b.payload as unknown as PlanPayload).entries ?? entries;
  }
  return entries;
});

const model = computed<{ rows: Row[]; toc: TocItem[] }>(() => {
  const rows: Row[] = [];
  const toc: TocItem[] = [];
  let n = 0;

  const sorted = [...blocks.values()].sort((a, b) => a.turn - b.turn || a.seq - b.seq);

  const usageBlocks = sorted.filter((b) => b.kind === 'usage');
  const usageAt = (turn: number): number | null => {
    let used: number | null = null;
    for (const u of usageBlocks) {
      if (u.turn <= turn) used = (u.payload as unknown as UsagePayload).used ?? used;
    }
    return used;
  };

  let census: ToolCallPayload[] = [];
  const flushCensus = () => {
    if (!census.length) return;
    rows.push({ type: 'census', key: `census-${census[0].tool_call_id}`, items: census });
    census = [];
  };

  for (const b of sorted) {
    const k = blockKey(b.turn, b.seq);
    switch (b.kind) {
      case 'user_message': {
        flushCensus();
        n += 1;
        const anchor = `acp-turn-${b.turn}`;
        const text = (b.payload as unknown as UserMessagePayload).text ?? '';
        rows.push({ type: 'user', key: k, anchor, n, time: shortTime(b.created_at), text });
        toc.push({ anchor, n, title: titleOf(text) });
        break;
      }
      case 'agent_message':
        flushCensus();
        rows.push({
          type: 'agent',
          key: k,
          time: shortTime(b.created_at),
          text: (b.payload as unknown as AgentMessagePayload).text ?? '',
          streaming: false,
        });
        break;
      case 'thought':
        flushCensus();
        rows.push({
          type: 'thought',
          key: k,
          text: (b.payload as unknown as ThoughtPayload).text ?? '',
          streaming: false,
        });
        break;
      case 'tool_call': {
        const tool = b.payload as unknown as ToolCallPayload;
        if (isQuiet(tool)) {
          census.push(tool);
        } else {
          flushCensus();
          rows.push({ type: 'card', key: k, tool, live: false });
        }
        break;
      }
      case 'permission_request':
        flushCensus();
        rows.push({ type: 'permission', key: k, perm: b.payload as unknown as PermissionPayload });
        break;
      case 'mode_change':
        flushCensus();
        rows.push({ type: 'mode', key: k, mode: (b.payload as { mode_id?: string }).mode_id ?? '' });
        break;
      case 'turn_end': {
        flushCensus();
        const stop = (b.payload as unknown as TurnEndPayload).stop_reason ?? 'end_turn';
        rows.push({
          type: 'turnRule',
          key: k,
          turn: b.turn,
          stop,
          ctx: usageAt(b.turn),
          loud: stop === 'refusal',
        });
        break;
      }
      // plan / usage: not rendered inline.
    }
  }
  flushCensus();

  // Trailing live content of the in-flight turn.
  const lt = liveTurnNo.value;
  if (lt != null) {
    const thought = shadows.get(`${lt}:thought`);
    if (thought && thought.text) {
      rows.push({ type: 'thought', key: `shadow-${lt}-thought`, text: thought.text, streaming: true });
    }
    const msg = shadows.get(`${lt}:agent_message`);
    if (msg && msg.text) {
      rows.push({ type: 'agent', key: `shadow-${lt}-agent`, time: '', text: msg.text, streaming: true });
    }
    for (const t of liveTools.values()) {
      if (t.turn !== lt) continue;
      rows.push({ type: 'card', key: `live-${t.tool_call_id}`, tool: t as unknown as ToolCallPayload, live: true });
    }
  }

  return { rows, toc };
});

// ── Presentational helpers ───────────────────────────────────────────────────
function toolGlyph(kind: string): string {
  return (
    { edit: '✎', execute: '⌗', delete: '✕', move: '⇄', read: '❏', search: '⌕', fetch: '⤓', think: '✳' } as Record<
      string,
      string
    >
  )[kind] ?? '•';
}
// The kind census on a collapsed run: `7 read · 2 search`, commonest first.
function censusBreakdown(items: ToolCallPayload[]): string {
  const counts = new Map<string, number>();
  for (const it of items) counts.set(it.tool_kind || 'other', (counts.get(it.tool_kind || 'other') ?? 0) + 1);
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([kind, c]) => `${c} ${kind}`)
    .join(' · ');
}
interface DiffLine {
  sign: '-' | '+';
  text: string;
}
// A diff content block rendered as ±diff lines.
function diffLines(c: Extract<ToolContent, { type: 'diff' }>): DiffLine[] {
  const lines: DiffLine[] = [];
  const push = (sign: '-' | '+', body: string) => {
    for (const l of body.replace(/\n$/, '').split('\n')) lines.push({ sign, text: l });
  };
  if (c.old) push('-', c.old);
  push('+', c.new);
  return lines;
}
function planGlyph(status: string): string {
  return status === 'completed' ? '✓' : status === 'in_progress' ? '▸' : '○';
}
function planTone(status: string): string {
  return status === 'completed' ? 'text-ok' : status === 'in_progress' ? 'text-agent' : 'text-faint';
}
function isAllow(kind: string): boolean {
  return kind.startsWith('allow');
}

// ── Folds ────────────────────────────────────────────────────────────────────
const open = ref<Set<string>>(new Set());
const isOpen = (k: string) => open.value.has(k);
function toggle(k: string) {
  const s = new Set(open.value);
  s.has(k) ? s.delete(k) : s.add(k);
  open.value = s;
}

// ── Working indicator (elapsed) ──────────────────────────────────────────────
const elapsed = ref(0);
let elapsedTimer: ReturnType<typeof setInterval> | null = null;
watch(turnLive, (live) => {
  if (elapsedTimer) {
    clearInterval(elapsedTimer);
    elapsedTimer = null;
  }
  if (live) {
    elapsed.value = 0;
    elapsedTimer = setInterval(() => (elapsed.value += 1), 1000);
  }
});
onUnmounted(() => {
  if (elapsedTimer) clearInterval(elapsedTimer);
});
const elapsedLabel = computed(() => {
  const m = Math.floor(elapsed.value / 60);
  const s = elapsed.value % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
});

// ── Jump-list scroll-spy ─────────────────────────────────────────────────────
const convScroll = ref<HTMLElement | null>(null);
const activeAnchor = ref('');
function nearBottom(): boolean {
  const el = convScroll.value;
  if (!el) return true;
  return el.scrollHeight - el.scrollTop - el.clientHeight < 120;
}
function scrollToBottom() {
  const el = convScroll.value;
  if (el) el.scrollTop = el.scrollHeight;
}
function autoFollow() {
  if (nearBottom()) nextTick(scrollToBottom);
}
function updateActive() {
  const root = convScroll.value;
  if (!root) return;
  const rootTop = root.getBoundingClientRect().top;
  const anchors = root.querySelectorAll<HTMLElement>('[data-anchor]');
  let current = anchors[0]?.dataset.anchor ?? '';
  for (const el of anchors) {
    if (el.getBoundingClientRect().top - rootTop <= 72) current = el.dataset.anchor ?? current;
    else break;
  }
  if (current) activeAnchor.value = current;
}
watch(
  () => model.value.rows.length,
  () => nextTick(updateActive),
);
function goTo(anchor: string) {
  activeAnchor.value = anchor;
  convScroll.value
    ?.querySelector(`[data-anchor="${CSS.escape(anchor)}"]`)
    ?.scrollIntoView({ behavior: 'smooth', block: 'start' });
}
</script>

<template>
  <div class="flex h-full flex-col">
    <p v-if="state === 'loading'" class="text-sm text-muted">Loading conversation…</p>
    <p v-else-if="state === 'error'" class="text-sm text-block">{{ errorMsg }}</p>

    <div v-else class="flex min-h-0 flex-1 gap-4">
      <!-- The transcript. -->
      <div
        ref="convScroll"
        data-testid="acp-conversation"
        class="acp-scroll min-h-0 flex-1 overflow-auto pb-8 pr-1"
        @scroll.passive="updateActive"
      >
        <template v-for="row in model.rows" :key="row.key">
          <!-- Turn rule — dashed hairline between turns. -->
          <div
            v-if="row.type === 'turnRule'"
            class="acp-turn-rule"
            :class="{ loud: row.loud }"
            data-testid="acp-turn-rule"
          >
            <span
              >turn {{ row.turn + 1 }} · {{ row.stop
              }}<template v-if="row.ctx != null"> · {{ Math.round(row.ctx / 1000) }}k ctx</template></span
            >
          </div>

          <!-- YOU — the human turn. -->
          <section
            v-else-if="row.type === 'user'"
            :id="row.anchor"
            :data-anchor="row.anchor"
            class="acp-speaker"
          >
            <header class="acp-rule">
              <span class="acp-label text-accent">You</span>
              <span class="acp-time">{{ row.time }}</span>
            </header>
            <MarkdownView :id="id" path="" :source="row.text" />
          </section>

          <!-- AGENT — the model's prose. -->
          <section v-else-if="row.type === 'agent'" class="acp-speaker">
            <header class="acp-rule">
              <span class="acp-label">Agent</span>
              <span v-if="row.time" class="acp-time">{{ row.time }}</span>
            </header>
            <MarkdownView :id="id" path="" :source="row.text" />
          </section>

          <!-- Thinking — a faint italic-serif fold. -->
          <div v-else-if="row.type === 'thought'" class="acp-thought" data-testid="acp-thought">
            <button type="button" class="acp-fold-head" @click="toggle(row.key)">
              <span class="chev" :class="{ open: isOpen(row.key) }">▸</span>
              <span>thinking</span>
            </button>
            <p v-if="isOpen(row.key)" class="acp-thought-body">{{ row.text }}</p>
          </div>

          <!-- Apparatus: a collapsed census of quiet calls. -->
          <div v-else-if="row.type === 'census'" class="acp-census" data-testid="acp-census">
            <button type="button" class="acp-fold-head" @click="toggle(row.key)">
              <span class="chev" :class="{ open: isOpen(row.key) }">▸</span>
              <span
                >{{ row.items.length }} {{ row.items.length === 1 ? 'call' : 'calls' }} —
                {{ censusBreakdown(row.items) }}</span
              >
            </button>
            <ul v-if="isOpen(row.key)" class="acp-census-list">
              <li v-for="(it, i) in row.items" :key="i">
                <span class="acp-tool-glyph">{{ toolGlyph(it.tool_kind) }}</span>
                <span class="truncate">{{ it.title || it.tool_kind }}</span>
              </li>
            </ul>
          </div>

          <!-- A consequential tool call — a standalone card. -->
          <div
            v-else-if="row.type === 'card'"
            class="acp-card"
            :class="{ 'acp-card-failed': row.tool.status === 'failed' }"
            data-testid="acp-card"
          >
            <div class="acp-card-head">
              <span class="acp-tool-glyph">{{ toolGlyph(row.tool.tool_kind) }}</span>
              <span class="acp-tool-title">{{ row.tool.title || row.tool.tool_kind }}</span>
              <span v-if="row.live" class="acp-live" data-testid="acp-card-live">▸ live</span>
              <span v-else class="acp-tool-status" :class="{ 'text-block': row.tool.status === 'failed' }">{{
                row.tool.status
              }}</span>
            </div>
            <div v-for="(c, ci) in row.tool.content" :key="ci" class="acp-payload-wrap">
              <!-- A diff renders as real ±diff lines. -->
              <pre v-if="c.type === 'diff'" class="acp-diff" data-testid="acp-diff"><code
                v-for="(l, li) in diffLines(c)"
                :key="li"
                class="acp-diff-line"
                :class="l.sign === '-' ? 'acp-diff-del' : 'acp-diff-add'"
              >{{ l.sign }} {{ l.text }}
</code></pre>
              <!-- Text / command output on the recessed panel tone. -->
              <pre v-else-if="c.type === 'text' && c.text" class="acp-payload">{{ c.text }}</pre>
            </div>
          </div>

          <!-- Permission — the one interactive block. -->
          <div v-else-if="row.type === 'permission'" class="acp-perm" data-testid="acp-permission">
            <div class="acp-perm-label">Permission</div>
            <p class="acp-perm-title">{{ row.perm.title }}</p>
            <div v-if="row.perm.outcome" class="acp-perm-receipt" data-testid="acp-permission-receipt">
              {{ row.perm.outcome.option_id }} · {{ shortTime(row.perm.outcome.at) }}
            </div>
            <div v-else class="acp-perm-options">
              <button
                v-for="opt in row.perm.options"
                :key="opt.option_id"
                type="button"
                :class="isAllow(opt.kind) ? 'btn-primary' : 'btn-secondary'"
                class="px-2.5 py-1 text-xs"
                :disabled="answering.has(row.perm.request_id)"
                data-testid="acp-permission-option"
                @click="answer(row.perm, opt.option_id)"
              >
                {{ opt.name }}
              </button>
            </div>
          </div>

          <!-- Mode change — a quiet centred marker. -->
          <div v-else-if="row.type === 'mode'" class="acp-mode-note">mode → {{ modeLabel(row.mode) }}</div>
        </template>

        <!-- Optimistic (in-flight / queued) user messages. -->
        <section
          v-for="(o, i) in optimistic"
          :key="`opt-${i}`"
          class="acp-speaker"
          data-testid="acp-optimistic"
        >
          <header class="acp-rule">
            <span class="acp-label text-accent">You</span>
            <span v-if="o.queued" class="acp-queued" data-testid="acp-queued">queued for next turn</span>
          </header>
          <MarkdownView :id="id" path="" :source="o.text" />
        </section>
      </div>

      <!-- Right rail: user-turn jump list + the current plan. -->
      <nav
        v-if="model.toc.length || latestPlan.length"
        class="hidden w-56 shrink-0 flex-col overflow-auto border-l border-line pl-3 lg:flex"
        data-testid="acp-rail"
        aria-label="Turns and plan"
      >
        <template v-if="model.toc.length">
          <p class="acp-rail-head">Turns</p>
          <ul class="mb-4 space-y-0.5" data-testid="acp-turns">
            <li v-for="t in model.toc" :key="t.anchor">
              <button
                type="button"
                class="acp-toc-item"
                :data-active="activeAnchor === t.anchor"
                @click="goTo(t.anchor)"
              >
                <span class="acp-toc-num">{{ t.n }}</span>
                <span class="truncate">{{ t.title }}</span>
              </button>
            </li>
          </ul>
        </template>

        <template v-if="latestPlan.length">
          <p class="acp-rail-head">Plan</p>
          <ul class="space-y-1" data-testid="acp-plan">
            <li v-for="(e, i) in latestPlan" :key="i" class="acp-plan-item">
              <span class="acp-plan-glyph" :class="planTone(e.status)">{{ planGlyph(e.status) }}</span>
              <span :class="e.status === 'pending' ? 'text-faint' : 'text-muted'">{{ e.content }}</span>
            </li>
          </ul>
        </template>
      </nav>
    </div>

    <!-- Working indicator — a sage cue while a turn runs. -->
    <div v-if="turnLive" class="acp-working" data-testid="acp-working" role="status" aria-live="polite">
      <span>▶ working</span>
      <span v-if="liveTurnNo != null" class="acp-working-meta">· turn {{ liveTurnNo + 1 }} · {{ elapsedLabel }}</span>
    </div>

    <!-- Composer. -->
    <form
      v-if="composerVisible"
      class="acp-composer"
      data-testid="acp-composer"
      @submit.prevent="submitPrompt"
    >
      <p v-if="sendError" class="mb-1.5 text-xs text-block" data-testid="acp-composer-error">{{ sendError }}</p>
      <textarea
        v-model="draft"
        rows="2"
        :disabled="sending"
        placeholder="Message the agent…"
        data-testid="acp-composer-input"
        class="acp-input"
        @keydown.enter.exact.prevent="submitPrompt"
      ></textarea>
      <div class="acp-composer-actions">
        <!-- Mode chip + slash hint on the left. -->
        <div class="acp-composer-left">
          <div class="acp-mode-wrap" data-testid="acp-mode-chip">
            <button
              type="button"
              class="acp-mode-chip"
              :class="{ 'acp-mode-static': !modeInteractive }"
              :disabled="!modeInteractive"
              @click.stop="modeOpen = !modeOpen"
            >
              {{ modeLabel(currentMode) }}<span v-if="modeInteractive" class="acp-mode-caret">▾</span>
            </button>
            <ul v-if="modeOpen" class="acp-mode-menu" data-testid="acp-mode-menu">
              <li v-for="m in modeOptions" :key="m">
                <button
                  type="button"
                  class="acp-mode-item"
                  :data-active="m === currentMode"
                  @click="pickMode(m)"
                >
                  {{ modeLabel(m) }}
                </button>
              </li>
            </ul>
          </div>
          <span class="acp-slash-hint" aria-hidden="true">/ commands</span>
        </div>
        <div class="acp-composer-right">
          <button
            v-if="turnLive"
            type="button"
            class="btn-secondary px-3 py-1 text-xs"
            data-testid="acp-composer-stop"
            :disabled="stopping"
            @click="stopTurn"
          >
            Stop
          </button>
          <button
            type="submit"
            class="btn-primary px-3 py-1 text-sm"
            data-testid="acp-composer-send"
            :disabled="sending || !draft.trim()"
          >
            {{ sending ? 'Sending…' : 'Send' }}
          </button>
        </div>
      </div>
    </form>
  </div>
</template>

<style scoped>
/* Flatten MarkdownView's card into tight, left-aligned serif prose on the
   canvas — the transcript reads as printed dialogue, not stacked cards. */
.acp-scroll :deep(div:has(> .markdown-body)) {
  background: transparent;
  overflow: visible;
}
.acp-scroll :deep(.markdown-body) {
  max-width: 46rem;
  margin: 0;
  padding: 0.125rem 0;
}

/* Speaker block: a hairline rule + micro-caps label + mono time, serif beneath. */
.acp-speaker {
  scroll-margin-top: 0.5rem;
  margin-top: 1.5rem;
}
.acp-speaker:first-child {
  margin-top: 0.25rem;
}
.acp-rule {
  display: flex;
  align-items: center;
  gap: 0.625rem;
  border-top: 1px solid var(--line);
  padding-top: 0.375rem;
  margin-bottom: 0.5rem;
}
.acp-label {
  font-family: var(--font-sans);
  font-size: 0.6875rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--muted);
}
.acp-time {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--faint);
  font-variant-numeric: tabular-nums;
}
.acp-queued {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--attn);
}

/* Thought fold. */
.acp-thought,
.acp-census {
  margin-top: 0.625rem;
}
.acp-fold-head {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--faint);
  cursor: pointer;
}
.acp-fold-head:hover {
  color: var(--muted);
}
.chev {
  display: inline-block;
  transition: transform 0.12s ease;
}
.chev.open {
  transform: rotate(90deg);
}
.acp-thought-body {
  margin: 0.3rem 0 0 1.15rem;
  font-family: var(--font-serif);
  font-style: italic;
  font-size: 0.8125rem;
  line-height: 1.45;
  color: var(--faint);
  white-space: pre-wrap;
}

/* Census expansion. */
.acp-census-list {
  margin: 0.3rem 0 0 1.15rem;
  display: flex;
  flex-direction: column;
  gap: 0.2rem;
}
.acp-census-list li {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--muted);
  min-width: 0;
}

/* Consequential tool card. */
.acp-card {
  margin-top: 0.75rem;
  border: 1px solid var(--line);
  border-radius: 0.375rem;
  overflow: hidden;
}
.acp-card-failed {
  border-left: 2px solid var(--block-line);
}
.acp-card-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.4rem 0.65rem;
  background: var(--surface);
  font-family: var(--font-mono);
  font-size: 0.75rem;
}
.acp-tool-glyph {
  flex: none;
  color: var(--muted);
}
.acp-tool-title {
  color: var(--fg);
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.acp-tool-status {
  margin-left: auto;
  flex: none;
  color: var(--faint);
}
.acp-live {
  margin-left: auto;
  flex: none;
  color: var(--ok);
}
.acp-payload {
  margin: 0;
  padding: 0.55rem 0.65rem;
  background: var(--code);
  color: var(--code-fg);
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.2rem;
  white-space: pre-wrap;
  overflow-x: auto;
  max-height: 22rem;
}
.acp-payload-wrap + .acp-payload-wrap .acp-payload,
.acp-payload-wrap + .acp-payload-wrap .acp-diff {
  border-top: 1px solid var(--line);
}

/* Diff — ±lines on the recessed tone. */
.acp-diff {
  margin: 0;
  padding: 0.4rem 0;
  background: var(--code);
  overflow-x: auto;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.2rem;
}
.acp-diff-line {
  display: block;
  padding: 0 0.65rem;
  white-space: pre;
}
.acp-diff-del {
  background: var(--block-soft);
  color: var(--block);
}
.acp-diff-add {
  background: var(--ok-soft);
  color: var(--ok);
}

/* Permission card — ochre rule + attention wash; the interface asking (sans). */
.acp-perm {
  margin-top: 0.85rem;
  border: 1px solid var(--line);
  border-left: 2px solid var(--attn-line);
  border-radius: 0.375rem;
  background: var(--attn-soft);
  padding: 0.65rem 0.8rem;
}
.acp-perm-label {
  font-family: var(--font-sans);
  font-size: 0.6875rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--attn);
}
.acp-perm-title {
  margin-top: 0.3rem;
  font-family: var(--font-serif);
  font-size: 0.9375rem;
  color: var(--fg);
}
.acp-perm-options {
  margin-top: 0.6rem;
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}
.acp-perm-receipt {
  margin-top: 0.4rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--muted);
}

/* Mode-change marker. */
.acp-mode-note {
  margin: 0.65rem 0 0;
  text-align: center;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--faint);
}

/* Turn rule. */
.acp-turn-rule {
  display: flex;
  align-items: center;
  justify-content: center;
  margin: 1.5rem 0 0.25rem;
  border-top: 1px dashed var(--line);
  padding-top: 0.55rem;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--faint);
  font-variant-numeric: tabular-nums;
}
.acp-turn-rule.loud {
  color: var(--block);
  border-top-color: var(--block-line);
}
.acp-turn-rule:first-child {
  margin-top: 0.25rem;
}

/* Working indicator. */
.acp-working {
  margin-top: 0.6rem;
  display: flex;
  align-items: center;
  gap: 0.4rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--ok);
  font-variant-numeric: tabular-nums;
}
.acp-working-meta {
  color: var(--faint);
}

/* Composer. */
.acp-composer {
  margin-top: 0.75rem;
  border-top: 1px solid var(--line);
  padding-top: 0.75rem;
}
.acp-input {
  width: 100%;
  resize: vertical;
  max-height: 12rem;
  border-radius: 0.25rem;
  background: var(--input);
  padding: 0.55rem 0.7rem;
  font-family: var(--font-serif);
  font-size: 0.9375rem;
  line-height: 1.5;
  outline: none;
}
.acp-input:focus {
  box-shadow: 0 0 0 1px var(--accent);
}
.acp-composer-actions {
  margin-top: 0.55rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
}
.acp-composer-left {
  display: flex;
  align-items: center;
  gap: 0.65rem;
  min-width: 0;
}
.acp-composer-right {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex: none;
}

/* Mode chip + dropdown. */
.acp-mode-wrap {
  position: relative;
}
.acp-mode-chip {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  border: 1px solid var(--line);
  border-radius: 0.25rem;
  background: var(--subtle);
  padding: 0.2rem 0.5rem;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--muted);
  cursor: pointer;
}
.acp-mode-chip:hover:not(:disabled) {
  color: var(--fg);
  background: var(--subtle-hover);
}
.acp-mode-static {
  cursor: default;
}
.acp-mode-caret {
  color: var(--faint);
}
.acp-mode-menu {
  position: absolute;
  bottom: calc(100% + 0.3rem);
  left: 0;
  z-index: 20;
  min-width: 9rem;
  border: 1px solid var(--line);
  border-radius: 0.375rem;
  background: var(--surface);
  box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
  padding: 0.2rem;
}
.acp-mode-item {
  display: block;
  width: 100%;
  border-radius: 0.2rem;
  padding: 0.3rem 0.5rem;
  text-align: left;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--muted);
  cursor: pointer;
}
.acp-mode-item:hover {
  background: var(--subtle);
  color: var(--fg);
}
.acp-mode-item[data-active='true'] {
  color: var(--accent);
}
.acp-slash-hint {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--faint);
}

/* Right rail. */
.acp-rail-head {
  margin-bottom: 0.5rem;
  padding-left: 0.25rem;
  font-family: var(--font-sans);
  font-size: 0.6875rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--faint);
}
.acp-toc-item {
  display: flex;
  width: 100%;
  align-items: baseline;
  gap: 0.5rem;
  border-radius: 0.25rem;
  border-left: 2px solid transparent;
  padding: 0.19rem 0.5rem;
  text-align: left;
  font-size: 0.75rem;
  line-height: 1.15rem;
  color: var(--muted);
  cursor: pointer;
}
.acp-toc-item:hover {
  background: color-mix(in srgb, var(--subtle) 55%, transparent);
  color: var(--fg);
}
.acp-toc-item[data-active='true'] {
  background: var(--subtle);
  border-left-color: var(--accent);
  color: var(--fg);
}
.acp-toc-num {
  flex: none;
  font-family: var(--font-mono);
  font-size: 0.625rem;
  color: var(--faint);
}
.acp-plan-item {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
  padding: 0 0.5rem;
  font-size: 0.75rem;
  line-height: 1.2rem;
}
.acp-plan-glyph {
  flex: none;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
}
</style>
