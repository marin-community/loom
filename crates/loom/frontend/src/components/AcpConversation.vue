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
  forceQueuedSession,
  interruptSession,
  answerPermission,
  setSessionMode,
  setSessionConfigOption,
  listSessionFiles,
} from '../api';
import type {
  Session,
  ChatBlock,
  SseDelta,
  SseTool,
  SseTurn,
  SseQueue,
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
  AcpMetadata,
  AcpCommand,
  AcpConfigOption,
  AcpConfigChoice,
  HandoffPayload,
} from '../types';
import { canSend } from '../lib/sessionState';
import { useFollowFoot } from '../lib/followFoot';
import MarkdownView from './MarkdownView.vue';

// The Conversation surface for an *ACP* session (`protocol='acp'`): typeset
// dialogue, not chat bubbles. Serif prose for the humans and the agent; the
// machine's apparatus recedes — every run of tool calls folds to one quiet
// activity line (closed by default, a failure re-opens it), and the in-flight
// turn reads as a live status line at the transcript's tail (the current tool,
// "Thinking…" while reasoning streams) rather than bare card churn.
//
// Its data source is the durable chat journal: it paints the `GET /chat`
// snapshot, then applies the `/chat/stream` SSE tail in place — `block` upserts
// by (turn, seq), `delta` streams into a shadow message/thought, `tool` tracks
// live tool state (feeding the status line), `turn` drives the live-turn state,
// and `queue` carries the complete durable next-turn prompt.
// The composer posts to `/prompt`; Stop posts `/interrupt`; permission cards
// answer via `/permissions/{id}`; the mode chip drives `/mode`.
const props = withDefaults(
  defineProps<{
    session: Session;
    /** Client-local commands owned by the embedding surface (Chat supplies
     * `/clear`; everything else comes from the ACP agent). */
    localCommands?: AcpCommand[];
  }>(),
  { localCommands: () => [] },
);
const emit = defineEmits<{ command: [name: string, args: string] }>();
const id = computed(() => props.session.id);

// ── The render state the stream feeds ────────────────────────────────────────
const blocks = reactive(new Map<string, ChatBlock>());
const shadows = reactive(
  new Map<string, { turn: number; kind: 'agent_message' | 'thought'; text: string }>(),
);
const liveTools = reactive(new Map<string, SseTool>());
const turnLive = ref(false);
const liveTurnNo = ref<number | null>(null);
// Optimistic rows exist only while an HTTP send is unresolved. Delivery state
// comes from the durable queue snapshot/SSE, never from the click that initiated
// the request.
const optimistic = ref<{ key: number; text: string }[]>([]);
const pendingPrompt = ref<string | null>(null);
let nextOptimisticKey = 0;
const emptyMetadata = (): AcpMetadata => ({
  commands: [],
  config_options: [],
  modes: [],
  steering_supported: false,
});
const metadata = ref<AcpMetadata>(emptyMetadata());
const steeringSupported = computed(() => metadata.value.steering_supported);

// The live mode, seeded from the session and advanced by `mode_change` blocks or
// a local set — so the composer chip reads true without a refetch.
const currentMode = ref<string | null>(props.session.current_mode);
const effectiveMode = ref<string | null>(null);
watch(
  () => props.session.current_mode,
  (m) => {
    if (m) currentMode.value = m;
  },
);

function applyMetadata(next: AcpMetadata) {
  metadata.value = next;
  // Config options are the state acknowledged or pushed by the adapter. Keep
  // the permissions chip aligned even when the agent changes it unsolicited.
  const mode = next.config_options.find(isModeOption)?.currentValue;
  if (typeof mode === 'string') currentMode.value = mode;
}

const blockKey = (turn: number, seq: number) => `${turn}:${seq}`;

type LoadState = 'loading' | 'ready' | 'error';
const state = ref<LoadState>('loading');
const errorMsg = ref('');

let loadSeq = 0;
async function load({ preserve = false }: { preserve?: boolean } = {}) {
  const seq = ++loadSeq;
  // A fresh load (mount / session switch) re-pins the view to the foot — a chat
  // opens at its newest exchange; a preserved refresh keeps the reader's pin.
  if (!preserve) {
    state.value = 'loading';
    pinned.value = true;
  }
  try {
    const snap = await getSessionChat(id.value);
    if (seq !== loadSeq) return;
    blocks.clear();
    for (const b of snap.blocks) blocks.set(blockKey(b.turn, b.seq), b);
    shadows.clear();
    liveTools.clear();
    liveTurnNo.value = snap.live_turn;
    turnLive.value = snap.live_turn != null;
    effectiveMode.value = snap.effective_mode ?? null;
    pendingPrompt.value = snap.pending_prompt?.trim() ? snap.pending_prompt : null;
    applyMetadata(snap.metadata ?? emptyMetadata());
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
  if (pinned.value) scrollToBottom();
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
    effectiveMode.value = ev.effective_mode ?? null;
    autoFollow();
  } else {
    turnLive.value = false;
    liveTurnNo.value = null;
    effectiveMode.value = null;
  }
}

function onQueue(ev: SseQueue) {
  pendingPrompt.value = ev.pending_prompt?.trim() ? ev.pending_prompt : null;
  autoFollow();
}

// ── SSE lifecycle (kept-alive discipline) ────────────────────────────────────
let source: EventSource | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
function openStream() {
  const stream = new EventSource(`/api/sessions/${id.value}/chat/stream`);
  source = stream;
  stream.addEventListener('block', (e) =>
    onBlock(JSON.parse((e as MessageEvent).data) as ChatBlock),
  );
  stream.addEventListener('delta', (e) =>
    onDelta(JSON.parse((e as MessageEvent).data) as SseDelta),
  );
  stream.addEventListener('tool', (e) => onTool(JSON.parse((e as MessageEvent).data) as SseTool));
  stream.addEventListener('turn', (e) => onTurn(JSON.parse((e as MessageEvent).data) as SseTurn));
  stream.addEventListener('queue', (e) =>
    onQueue(JSON.parse((e as MessageEvent).data) as SseQueue),
  );
  stream.addEventListener('metadata', (e) => {
    applyMetadata(JSON.parse((e as MessageEvent).data) as AcpMetadata);
  });
  // Once the server has installed the subscription, reconcile from the durable
  // journal. Subscribing first avoids a snapshot-to-stream gap during handoff.
  stream.addEventListener('open', () => load({ preserve: true }));
  // A provider handoff cleanly closes the old task's broadcast. Browsers do not
  // consistently reconnect an EventSource after a clean EOF, so explicitly
  // replace it and refetch the durable snapshot before tailing the new task.
  stream.onerror = () => {
    if (source !== stream) return;
    stream.close();
    source = null;
    if (reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (!source) openStream();
    }, 100);
  };
}
function closeStream() {
  if (reconnectTimer) clearTimeout(reconnectTimer);
  reconnectTimer = null;
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
const composerInput = ref<HTMLTextAreaElement | null>(null);
const sending = ref(false);
const sendError = ref('');
const composerVisible = computed(() => canSend(props.session));
const selectedFiles = ref<string[]>([]);

async function submitPrompt(forceSteer = false) {
  if (!draft.value.trim() || sending.value) return;
  const local = localCommand(draft.value);
  if (local) {
    draft.value = '';
    emit('command', local.name, local.args);
    return;
  }
  sending.value = true;
  sendError.value = '';
  const text = draft.value;
  const pending = { key: nextOptimisticKey++, text };
  optimistic.value.push(pending);
  autoFollow();
  try {
    const files = selectedFiles.value.filter((path) => text.includes(`@${path}`));
    await promptSession(id.value, text, undefined, forceSteer, files);
    const index = optimistic.value.findIndex((o) => o.key === pending.key);
    if (index >= 0) optimistic.value.splice(index, 1);
    draft.value = '';
    selectedFiles.value = [];
    // Reconcile against the journal + durable queue that produced the ack. SSE
    // covers other clients; this closes the request/event race for this client.
    await load({ preserve: true });
    autoFollow();
  } catch (e) {
    const index = optimistic.value.findIndex((o) => o.key === pending.key);
    if (index >= 0) optimistic.value.splice(index, 1);
    sendError.value = (e as Error).message ?? 'Failed to send';
  } finally {
    sending.value = false;
  }
}

async function forceQueued() {
  if (sending.value) return;
  sending.value = true;
  sendError.value = '';
  try {
    await forceQueuedSession(id.value);
    await load({ preserve: true });
    autoFollow();
  } catch (e) {
    sendError.value = (e as Error).message ?? 'Failed to send queued feedback';
  } finally {
    sending.value = false;
  }
}

// ── Agent-owned slash commands ---------------------------------------------
// The adapter replaces its command catalogue at runtime. Local surface hooks
// win on a duplicate name, then the remainder is
// filtered as the user types. Selecting a command with input leaves a trailing
// space and exposes the adapter's hint in the composer placeholder.
const commands = computed<AcpCommand[]>(() => {
  const seen = new Set<string>();
  const out: AcpCommand[] = [];
  for (const command of [...props.localCommands, ...metadata.value.commands]) {
    if (!command.name || seen.has(command.name)) continue;
    seen.add(command.name);
    out.push(command);
  }
  return out;
});
const slashQuery = computed(() => draft.value.match(/^\/([^\s/]*)$/)?.[1] ?? null);
const commandMatches = computed(() => {
  if (slashQuery.value == null) return [];
  const q = slashQuery.value.toLowerCase();
  return commands.value.filter((command) => command.name.toLowerCase().includes(q)).slice(0, 10);
});
const commandIndex = ref(0);
watch(slashQuery, () => (commandIndex.value = 0));
const activeCommand = computed(() => commandMatches.value[commandIndex.value] ?? null);
const commandHint = computed(() => {
  const match = draft.value.match(/^\/([^\s]+)\s*$/);
  if (!match) return '';
  return commands.value.find((command) => command.name === match[1])?.input?.hint ?? '';
});

// ── Server-backed @file completion ----------------------------------------
// The browser cannot inspect a session worktree. Once the current token starts
// with `@`, ask loom for tracked/unignored paths; a chosen path is also sent as
// an ACP resource_link rather than relying on adapter-specific text parsing.
const fileQuery = computed(() => draft.value.match(/(?:^|[\s|])@([^\s@]*)$/)?.[1] ?? null);
const fileMatches = ref<string[]>([]);
const fileIndex = ref(0);
let fileSearchSeq = 0;
let fileSearchTimer: ReturnType<typeof setTimeout> | null = null;
watch(fileQuery, (query) => {
  fileIndex.value = 0;
  const seq = ++fileSearchSeq;
  if (fileSearchTimer) {
    clearTimeout(fileSearchTimer);
    fileSearchTimer = null;
  }
  if (query == null) {
    fileMatches.value = [];
    return;
  }
  fileSearchTimer = setTimeout(async () => {
    fileSearchTimer = null;
    try {
      const result = await listSessionFiles(id.value, query);
      if (seq === fileSearchSeq) fileMatches.value = result.files;
    } catch {
      if (seq === fileSearchSeq) fileMatches.value = [];
    }
  }, 150);
});
onUnmounted(() => {
  if (fileSearchTimer) clearTimeout(fileSearchTimer);
});
const activeFile = computed(() => fileMatches.value[fileIndex.value] ?? null);

function chooseFile(path: string) {
  draft.value = draft.value.replace(/@[^\s@]*$/, `@${path} `);
  if (!selectedFiles.value.includes(path)) selectedFiles.value.push(path);
  fileMatches.value = [];
  nextTick(() => composerInput.value?.focus());
}

function chooseCommand(command: AcpCommand) {
  // Keep a trailing space even for argument-less commands. It closes the
  // autocomplete so the next Enter submits instead of selecting forever.
  draft.value = `/${command.name} `;
  nextTick(() => composerInput.value?.focus());
}
function showCommands() {
  draft.value = '/';
  nextTick(() => composerInput.value?.focus());
}
function localCommand(text: string): { name: string; args: string } | null {
  const match = text.trim().match(/^\/([^\s]+)(?:\s+(.*))?$/s);
  if (!match || !props.localCommands.some((command) => command.name === match[1])) return null;
  return { name: match[1], args: match[2] ?? '' };
}
function onComposerKeydown(e: KeyboardEvent) {
  if (fileMatches.value.length) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const d = e.key === 'ArrowDown' ? 1 : -1;
      fileIndex.value = (fileIndex.value + d + fileMatches.value.length) % fileMatches.value.length;
      return;
    }
    if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
      e.preventDefault();
      if (activeFile.value) chooseFile(activeFile.value);
      return;
    }
  }
  if (commandMatches.value.length) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const d = e.key === 'ArrowDown' ? 1 : -1;
      commandIndex.value =
        (commandIndex.value + d + commandMatches.value.length) % commandMatches.value.length;
      return;
    }
    if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
      e.preventDefault();
      if (activeCommand.value) chooseCommand(activeCommand.value);
      return;
    }
  }
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    submitPrompt();
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
// The well-known claude/codex ACP modes, used until the adapter advertises its
// live mode catalogue through the conversation metadata.
const KNOWN_MODES = ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions'];
const MODE_LABEL: Record<string, string> = {
  auto: 'auto',
  default: 'default',
  acceptEdits: 'accept edits',
  plan: 'plan',
  bypassPermissions: 'bypass',
};
const modeOptions = computed(() =>
  metadata.value.modes.length ? metadata.value.modes.map((mode) => mode.id) : KNOWN_MODES,
);
const modeLabel = (m: string | null) => {
  if (!m) return 'mode';
  return metadata.value.modes.find((mode) => mode.id === m)?.name ?? MODE_LABEL[m] ?? m;
};
const modeInteractive = computed(() => canSend(props.session) && modeOptions.value.length > 1);
const legacyModeVisible = computed(() => !metadata.value.config_options.some(isModeOption));
const modeOpen = ref(false);
async function pickMode(m: string) {
  modeOpen.value = false;
  if (m === currentMode.value) return;
  try {
    await setSessionMode(id.value, m);
    currentMode.value = m;
  } catch (e) {
    sendError.value = (e as Error).message ?? 'Failed to change agent permissions';
  }
}
function onDocClick(e: MouseEvent) {
  if (modeOpen.value && !(e.target as HTMLElement).closest('[data-testid="acp-mode-chip"]')) {
    modeOpen.value = false;
  }
  if (configOpen.value && !(e.target as HTMLElement).closest('[data-acp-config]')) {
    configOpen.value = '';
  }
}
onMounted(() => document.addEventListener('click', onDocClick));
onUnmounted(() => document.removeEventListener('click', onDocClick));

// ── Agent-owned model / reasoning / config controls -------------------------
const configOpen = ref('');
const configBusy = ref('');
const configOptions = computed(() =>
  metadata.value.config_options
    .filter(
      (option) =>
        (option.type === 'select' && typeof option.currentValue === 'string') ||
        (option.type === 'boolean' && typeof option.currentValue === 'boolean'),
    )
    .sort((a, b) => configRank(a) - configRank(b)),
);
function configRank(option: AcpConfigOption): number {
  if (isModeOption(option)) return 0;
  if (option.category === 'model' || option.id === 'model') return 1;
  if (option.category === 'thought_level' || option.id.includes('reasoning')) return 2;
  return 3;
}
function configName(option: AcpConfigOption): string {
  if (isModeOption(option)) return 'Permissions';
  if (option.category === 'thought_level') return 'Effort';
  return option.name;
}
function configTone(option: AcpConfigOption): string {
  if (!isModeOption(option)) return '';
  return option.currentValue === 'agent-full-access' || option.currentValue === 'bypassPermissions'
    ? 'acp-config-danger'
    : 'acp-config-permission';
}
function configChoices(option: AcpConfigOption): AcpConfigChoice[] {
  const choices = option.options ?? [];
  if (!choices.length) return [];
  if ('value' in choices[0]) return choices as AcpConfigChoice[];
  return (choices as { options: AcpConfigChoice[] }[]).flatMap((group) => group.options ?? []);
}
function configValueLabel(option: AcpConfigOption): string {
  if (typeof option.currentValue === 'boolean') return option.currentValue ? 'On' : 'Off';
  const current = String(option.currentValue);
  return configChoices(option).find((choice) => choice.value === current)?.name ?? current;
}
function selectionAppliesNextTurn(value: unknown): boolean {
  return (
    turnLive.value &&
    effectiveMode.value !== null &&
    typeof value === 'string' &&
    value !== effectiveMode.value
  );
}
function configAppliesNextTurn(option: AcpConfigOption): boolean {
  return isModeOption(option) && selectionAppliesNextTurn(option.currentValue);
}
const permissionModeAppliesNextTurn = computed(() => selectionAppliesNextTurn(currentMode.value));
async function pickConfig(option: AcpConfigOption, value: string | boolean) {
  configOpen.value = '';
  if (value === option.currentValue || configBusy.value) return;
  configBusy.value = option.id;
  try {
    const response = await setSessionConfigOption(id.value, option.id, value);
    // Clear the write lock before publishing the acknowledged value. Vue can
    // paint that value before this async function's `finally` runs; keeping the
    // lock through that paint makes an immediate click on the next pill vanish.
    configBusy.value = '';
    // The response is the full state acknowledged by the agent. This matters
    // when changing one value also changes another control's choices (a model
    // switch can alter its supported reasoning efforts).
    metadata.value = response.metadata;
    const mode = response.metadata.config_options.find(isModeOption)?.currentValue;
    if (typeof mode === 'string') currentMode.value = mode;
  } catch (e) {
    sendError.value = (e as Error).message ?? 'Failed to change agent configuration';
  } finally {
    configBusy.value = '';
  }
}

function isModeOption(option: AcpConfigOption): boolean {
  return option.category === 'mode' || option.id === 'mode';
}

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
// Every run of consecutive tool calls — quiet reads and consequential edits
// alike — folds into one *activity* group, closed by default; a group holding a
// failed call opens by default (and the failed call's output with it). A live
// (non-terminal) tool never renders as a row: it drives the status line until
// its terminal block journals into the group.

interface ActivityItem {
  tool: ToolCallPayload;
  failed: boolean;
}

type Row =
  | { type: 'turnRule'; key: string; turn: number; stop: string; ctx: number | null; loud: boolean }
  | {
      type: 'user';
      key: string;
      anchor: string;
      n: number;
      turn: number;
      time: string;
      text: string;
      steered: boolean;
    }
  | { type: 'agent'; key: string; time: string; text: string; streaming: boolean }
  | { type: 'thought'; key: string; text: string; streaming: boolean }
  | { type: 'activity'; key: string; items: ActivityItem[]; failures: number }
  | { type: 'permission'; key: string; perm: PermissionPayload }
  | { type: 'mode'; key: string; mode: string }
  | { type: 'handoff'; key: string; handoff: HandoffPayload };

interface TocItem {
  anchor: string;
  n: number;
  title: string;
}

function firstLine(s: string): string {
  return (
    s
      .split('\n')
      .map((l) => l.trim())
      .find(Boolean) ?? ''
  );
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

  let activity: ActivityItem[] = [];
  let activityKey = '';
  const flushActivity = () => {
    if (!activity.length) return;
    rows.push({
      type: 'activity',
      key: activityKey,
      items: activity,
      failures: activity.filter((i) => i.failed).length,
    });
    activity = [];
  };

  for (const b of sorted) {
    const k = blockKey(b.turn, b.seq);
    switch (b.kind) {
      case 'user_message': {
        flushActivity();
        const user = b.payload as unknown as UserMessagePayload;
        const steered = user.steered === true;
        if (!steered) n += 1;
        const anchor = steered ? `acp-steer-${b.turn}-${b.seq}` : `acp-turn-${b.turn}`;
        const text = user.text ?? '';
        rows.push({
          type: 'user',
          key: k,
          anchor,
          n,
          turn: b.turn,
          time: shortTime(b.created_at),
          text,
          steered,
        });
        if (!steered) toc.push({ anchor, n, title: titleOf(text) });
        break;
      }
      case 'agent_message': {
        flushActivity();
        const text = (b.payload as unknown as AgentMessagePayload).text ?? '';
        if (!text.trim()) break;
        const previous = rows[rows.length - 1];
        if (previous?.type === 'agent') {
          // Metadata updates can split one streamed answer into multiple durable
          // blocks. They are not speaker boundaries, so keep the transcript as
          // one readable response until a user/tool/other visible row intervenes.
          previous.text += text;
          break;
        }
        rows.push({
          type: 'agent',
          key: k,
          time: shortTime(b.created_at),
          text,
          streaming: false,
        });
        break;
      }
      case 'thought':
        flushActivity();
        rows.push({
          type: 'thought',
          key: k,
          text: (b.payload as unknown as ThoughtPayload).text ?? '',
          streaming: false,
        });
        break;
      case 'tool_call': {
        const tool = b.payload as unknown as ToolCallPayload;
        if (!activity.length) activityKey = `act-${k}`;
        activity.push({ tool, failed: tool.status === 'failed' });
        break;
      }
      case 'permission_request':
        flushActivity();
        rows.push({ type: 'permission', key: k, perm: b.payload as unknown as PermissionPayload });
        break;
      case 'mode_change':
        flushActivity();
        rows.push({
          type: 'mode',
          key: k,
          mode: (b.payload as { mode_id?: string }).mode_id ?? '',
        });
        break;
      case 'handoff':
        flushActivity();
        rows.push({
          type: 'handoff',
          key: k,
          handoff: b.payload as unknown as HandoffPayload,
        });
        break;
      case 'turn_end': {
        flushActivity();
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
  flushActivity();

  // Trailing live prose of the in-flight turn. A streaming thought renders open
  // (its tail visible); a streaming message renders as ordinary prose growing in
  // place. Live tools stay out of the flow — the status line carries them.
  const lt = liveTurnNo.value;
  if (lt != null) {
    const thought = shadows.get(`${lt}:thought`);
    if (thought && thought.text) {
      rows.push({
        type: 'thought',
        key: `shadow-${lt}-thought`,
        text: thought.text,
        streaming: true,
      });
    }
    const msg = shadows.get(`${lt}:agent_message`);
    if (msg && msg.text) {
      const previous = rows[rows.length - 1];
      if (previous?.type === 'agent') {
        previous.text += msg.text;
        previous.streaming = true;
      } else {
        rows.push({
          type: 'agent',
          key: `shadow-${lt}-agent`,
          time: '',
          text: msg.text,
          streaming: true,
        });
      }
    }
  }

  return { rows, toc };
});

// The empty conversation, styled on purpose — a fresh session has no journal
// yet, and a bare canvas reads as breakage.
const isEmpty = computed(
  () =>
    state.value === 'ready' &&
    !model.value.rows.length &&
    !optimistic.value.length &&
    !pendingPrompt.value &&
    !turnLive.value,
);

// ── Live status line ─────────────────────────────────────────────────────────
// What the agent is doing right now, named: the newest live tool's title, else
// "Thinking…" / "Writing…" while a thought / message streams, else "Working…".
const statusLabel = computed(() => {
  const lt = liveTurnNo.value;
  if (lt == null) return 'Working';
  let tool: SseTool | null = null;
  for (const t of liveTools.values()) {
    if (t.turn === lt) tool = t;
  }
  if (tool) return tool.title || tool.tool_kind;
  if (shadows.get(`${lt}:agent_message`)?.text) return 'Writing';
  if (shadows.get(`${lt}:thought`)?.text) return 'Thinking';
  return 'Working';
});

// ── Presentational helpers ───────────────────────────────────────────────────
function toolGlyph(kind: string): string {
  return (
    (
      {
        edit: '✎',
        execute: '⌗',
        delete: '✕',
        move: '⇄',
        read: '❏',
        search: '⌕',
        fetch: '⤓',
        think: '✳',
      } as Record<string, string>
    )[kind] ?? '•'
  );
}
// The kind census on a collapsed group: `7 read · 2 search`, commonest first.
function activityBreakdown(items: ActivityItem[]): string {
  const counts = new Map<string, number>();
  for (const it of items) {
    const kind = it.tool.tool_kind || 'other';
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([kind, c]) => `${c} ${kind}`)
    .join(' · ');
}
// Whether a call carries anything worth expanding (a diff or non-empty text).
function hasDetail(t: ToolCallPayload): boolean {
  return (t.content ?? []).some((c) => c.type === 'diff' || (c.type === 'text' && !!c.text));
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
  return status === 'completed'
    ? 'text-ok'
    : status === 'in_progress'
      ? 'text-agent'
      : 'text-faint';
}
function isAllow(kind: string): boolean {
  return kind.startsWith('allow');
}

// ── Folds ────────────────────────────────────────────────────────────────────
// A fold's default is contextual (a failed activity group / failed call opens by
// default; everything else closes); an explicit toggle overrides the default.
const folds = ref(new Map<string, boolean>());
function foldOpen(key: string, dflt = false): boolean {
  return folds.value.get(key) ?? dflt;
}
function toggleFold(key: string, dflt = false) {
  const m = new Map(folds.value);
  m.set(key, !foldOpen(key, dflt));
  folds.value = m;
}

// ── Working timer (elapsed) ──────────────────────────────────────────────────
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

// ── Follow-the-foot scroll (lib/followFoot.ts): pinned-at-the-newest-exchange. ──
const convScroll = ref<HTMLElement | null>(null);
const convBody = ref<HTMLElement | null>(null);
const { pinned, scrollToBottom, autoFollow, trackPin } = useFollowFoot(convScroll, convBody);
function onScroll() {
  trackPin();
  updateActive();
}

// ── Jump-list scroll-spy ─────────────────────────────────────────────────────
const activeAnchor = ref('');
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
        class="acp-scroll min-h-0 flex-1 overflow-auto pb-6 pr-1"
        @scroll.passive="onScroll"
      >
        <div ref="convBody">
          <!-- A fresh session: say so, instead of a blank canvas. -->
          <div v-if="isEmpty" class="acp-empty" data-testid="acp-empty">
            <p class="acp-empty-lede">No conversation yet</p>
            <p class="acp-empty-hint">
              {{
                composerVisible
                  ? 'The transcript appears when the agent takes its first turn — or start one with a message below.'
                  : 'The transcript appears when the agent takes its first turn.'
              }}
            </p>
          </div>

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
                }}<template v-if="row.ctx != null">
                  · {{ Math.round(row.ctx / 1000) }}k ctx</template
                ></span
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
                <span v-if="row.steered" class="acp-prompt-state" data-testid="acp-steered"
                  >steered turn {{ row.turn + 1 }}</span
                >
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

            <!-- Thinking — streaming shows its live tail (the status line below
                 names it); settled folds away. -->
            <div v-else-if="row.type === 'thought'" class="acp-thought" data-testid="acp-thought">
              <template v-if="row.streaming">
                <div class="acp-thought-live-clip">
                  <p class="acp-thought-body">{{ row.text }}</p>
                </div>
              </template>
              <template v-else>
                <button type="button" class="acp-fold-head" @click="toggleFold(row.key)">
                  <span class="chev" :class="{ open: foldOpen(row.key) }">▸</span>
                  <span>thinking</span>
                </button>
                <p v-if="foldOpen(row.key)" class="acp-thought-body">{{ row.text }}</p>
              </template>
            </div>

            <!-- Apparatus: one folded activity line per run of tool calls. -->
            <div
              v-else-if="row.type === 'activity'"
              class="acp-activity"
              data-testid="acp-activity"
            >
              <button
                type="button"
                class="acp-fold-head"
                data-testid="acp-activity-head"
                @click="toggleFold(row.key, row.failures > 0)"
              >
                <span class="chev" :class="{ open: foldOpen(row.key, row.failures > 0) }">▸</span>
                <span v-if="row.items.length === 1" class="acp-activity-solo">
                  <span class="acp-tool-glyph">{{ toolGlyph(row.items[0].tool.tool_kind) }}</span>
                  <span class="truncate">{{
                    row.items[0].tool.title || row.items[0].tool.tool_kind
                  }}</span>
                </span>
                <span v-else
                  >{{ row.items.length }} steps — {{ activityBreakdown(row.items) }}</span
                >
                <span
                  v-if="row.failures"
                  class="acp-activity-failbadge"
                  data-testid="acp-activity-failed"
                  >{{ row.failures }} failed</span
                >
              </button>
              <ul v-if="foldOpen(row.key, row.failures > 0)" class="acp-activity-list">
                <li
                  v-for="it in row.items"
                  :key="it.tool.tool_call_id"
                  data-testid="acp-activity-item"
                >
                  <button
                    type="button"
                    class="acp-activity-line"
                    :disabled="!hasDetail(it.tool)"
                    @click="toggleFold(`tool-${it.tool.tool_call_id}`, it.failed)"
                  >
                    <span class="acp-tool-glyph">{{ toolGlyph(it.tool.tool_kind) }}</span>
                    <span class="truncate">{{ it.tool.title || it.tool.tool_kind }}</span>
                    <span v-if="it.failed" class="acp-activity-status text-block">failed</span>
                    <span
                      v-else-if="hasDetail(it.tool)"
                      class="chev sm"
                      :class="{ open: foldOpen(`tool-${it.tool.tool_call_id}`, it.failed) }"
                      >▸</span
                    >
                  </button>
                  <div
                    v-if="hasDetail(it.tool) && foldOpen(`tool-${it.tool.tool_call_id}`, it.failed)"
                    class="acp-detail"
                    data-testid="acp-detail"
                  >
                    <template v-for="(c, ci) in it.tool.content" :key="ci">
                      <!-- A diff renders as real ±diff lines. -->
                      <pre v-if="c.type === 'diff'" class="acp-diff" data-testid="acp-diff"><code
                        v-for="(l, li) in diffLines(c)"
                        :key="li"
                        class="acp-diff-line"
                        :class="l.sign === '-' ? 'acp-diff-del' : 'acp-diff-add'"
                      >{{ l.sign }} {{ l.text }}
  </code></pre>
                      <!-- Text / command output on the recessed panel tone. -->
                      <pre v-else-if="c.type === 'text' && c.text" class="acp-payload">{{
                        c.text
                      }}</pre>
                    </template>
                  </div>
                </li>
              </ul>
            </div>

            <!-- Permission — the one interactive block. -->
            <div
              v-else-if="row.type === 'permission'"
              class="acp-perm"
              data-testid="acp-permission"
            >
              <div class="acp-perm-label">Permission</div>
              <p class="acp-perm-title">{{ row.perm.title }}</p>
              <div
                v-if="row.perm.outcome"
                class="acp-perm-receipt"
                data-testid="acp-permission-receipt"
              >
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
            <div v-else-if="row.type === 'mode'" class="acp-mode-note">
              mode → {{ modeLabel(row.mode) }}
            </div>

            <!-- Provider boundary — the injected bootstrap stays hidden; this
                 compact receipt is the honest journal provenance. -->
            <div v-else-if="row.type === 'handoff'" class="acp-mode-note" data-testid="acp-handoff">
              handoff · {{ row.handoff.from }} → {{ row.handoff.to
              }}<template v-if="row.handoff.model"> · {{ row.handoff.model }}</template>
            </div>
          </template>

          <!-- The server-owned durable queue is one coalesced next-turn prompt. -->
          <section v-if="pendingPrompt" class="acp-speaker" data-testid="acp-pending">
            <header class="acp-rule">
              <span class="acp-label text-accent">You</span>
              <span class="acp-prompt-state" data-testid="acp-queued"
                >queued · agent hasn’t seen this yet</span
              >
              <button
                type="button"
                class="acp-prompt-action"
                data-testid="acp-force-queued"
                :disabled="sending"
                :title="
                  turnLive
                    ? steeringSupported
                      ? 'Inject all queued feedback into the running turn now'
                      : 'Stop the running turn and send all queued feedback as the next turn'
                    : 'Start a new turn with all queued feedback'
                "
                @click="forceQueued"
              >
                {{ turnLive ? (steeringSupported ? 'Force now' : 'Stop & send') : 'Send now' }}
              </button>
            </header>
            <MarkdownView :id="id" path="" :source="pendingPrompt" />
          </section>

          <!-- A local send remains unclassified only until the server answers. -->
          <section
            v-for="o in optimistic"
            :key="`opt-${o.key}`"
            class="acp-speaker"
            data-testid="acp-optimistic"
          >
            <header class="acp-rule">
              <span class="acp-label text-accent">You</span>
            </header>
            <MarkdownView :id="id" path="" :source="o.text" />
          </section>

          <!-- Live status — what the agent is doing right now, at the tail. -->
          <div
            v-if="turnLive"
            class="acp-status"
            data-testid="acp-working"
            role="status"
            aria-live="polite"
          >
            <span class="acp-live-label">{{ statusLabel }}…</span>
            <span class="acp-status-meta"
              >turn {{ (liveTurnNo ?? 0) + 1 }} · {{ elapsedLabel }}</span
            >
          </div>
        </div>
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
              <span class="acp-plan-glyph" :class="planTone(e.status)">{{
                planGlyph(e.status)
              }}</span>
              <span :class="e.status === 'pending' ? 'text-faint' : 'text-muted'">{{
                e.content
              }}</span>
            </li>
          </ul>
        </template>
      </nav>
    </div>

    <!-- Composer. -->
    <form
      v-if="composerVisible"
      class="acp-composer"
      data-testid="acp-composer"
      @submit.prevent="submitPrompt(false)"
    >
      <p v-if="sendError" class="mb-1.5 text-xs text-block" data-testid="acp-composer-error">
        {{ sendError }}
      </p>
      <textarea
        ref="composerInput"
        v-model="draft"
        rows="2"
        :disabled="sending"
        :placeholder="commandHint || 'Message the agent…'"
        data-testid="acp-composer-input"
        class="acp-input"
        @keydown="onComposerKeydown"
      ></textarea>
      <ul
        v-if="fileMatches.length"
        class="acp-command-menu"
        data-testid="acp-file-menu"
        role="listbox"
        aria-label="Worktree files"
      >
        <li v-for="(path, index) in fileMatches" :key="path">
          <button
            type="button"
            class="acp-command-item"
            :data-active="index === fileIndex"
            :aria-selected="index === fileIndex"
            role="option"
            @click="chooseFile(path)"
          >
            <code>@{{ path }}</code>
          </button>
        </li>
      </ul>
      <ul
        v-if="commandMatches.length"
        class="acp-command-menu"
        data-testid="acp-command-menu"
        role="listbox"
        aria-label="Agent commands"
      >
        <li v-for="(command, index) in commandMatches" :key="command.name">
          <button
            type="button"
            class="acp-command-item"
            :data-active="index === commandIndex"
            :aria-selected="index === commandIndex"
            role="option"
            @click="chooseCommand(command)"
          >
            <code>/{{ command.name }}</code>
            <span>{{ command.description }}</span>
            <kbd v-if="command.input?.hint">{{ command.input.hint }}</kbd>
          </button>
        </li>
      </ul>
      <div class="acp-composer-actions">
        <!-- Agent-owned config selectors + slash discovery on the left. -->
        <div class="acp-composer-left">
          <div
            v-for="option in configOptions"
            :key="option.id"
            class="acp-mode-wrap"
            data-acp-config
            :data-testid="`acp-config-${option.id}`"
          >
            <button
              type="button"
              class="acp-mode-chip"
              :class="configTone(option)"
              :disabled="configBusy === option.id"
              :title="option.description ?? option.name"
              :aria-pressed="option.type === 'boolean' ? Boolean(option.currentValue) : undefined"
              @click.stop="
                option.type === 'boolean'
                  ? pickConfig(option, !option.currentValue)
                  : (configOpen = configOpen === option.id ? '' : option.id)
              "
            >
              <span class="acp-config-name">{{ configName(option) }}</span>
              {{ configValueLabel(option)
              }}<span v-if="configAppliesNextTurn(option)" class="acp-config-timing">next turn</span
              ><span v-if="option.type === 'select'" class="acp-mode-caret">▾</span>
            </button>
            <ul v-if="configOpen === option.id" class="acp-mode-menu" data-testid="acp-config-menu">
              <li v-for="choice in configChoices(option)" :key="choice.value">
                <button
                  type="button"
                  class="acp-mode-item"
                  :data-active="choice.value === option.currentValue"
                  :title="choice.description ?? undefined"
                  @click="pickConfig(option, choice.value)"
                >
                  {{ choice.name }}
                </button>
              </li>
            </ul>
          </div>
          <div v-if="legacyModeVisible" class="acp-mode-wrap" data-testid="acp-mode-chip">
            <button
              type="button"
              class="acp-mode-chip"
              :class="{ 'acp-mode-static': !modeInteractive }"
              :disabled="!modeInteractive"
              @click.stop="modeOpen = !modeOpen"
            >
              <span class="acp-config-name">Permissions</span>
              {{ modeLabel(currentMode)
              }}<span v-if="permissionModeAppliesNextTurn" class="acp-config-timing">next turn</span
              ><span v-if="modeInteractive" class="acp-mode-caret">▾</span>
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
          <button
            v-if="commands.length"
            type="button"
            class="acp-slash-hint"
            data-testid="acp-command-hint"
            title="Show commands"
            @click="showCommands"
          >
            / {{ commands.length }} commands
          </button>
        </div>
        <div class="acp-composer-right">
          <button
            v-if="turnLive && steeringSupported"
            type="button"
            class="btn-secondary px-3 py-1 text-xs"
            data-testid="acp-composer-force-steer"
            :disabled="sending || !draft.trim()"
            title="Inject this feedback into the running turn"
            @click="submitPrompt(true)"
          >
            Force steer
          </button>
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
   canvas — the transcript reads as printed dialogue, not stacked cards. The
   denser chat rhythm (leading, block gaps, list indents) is the shared
   `chat-prose` layer in markdown.css; only this surface's geometry lives here. */
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
  margin-top: 1rem;
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
  margin-bottom: 0.375rem;
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
.acp-prompt-state {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--attn);
}
.acp-prompt-action {
  border-bottom: 1px solid currentColor;
  color: var(--accent);
  font-family: var(--font-sans);
  font-size: 0.6875rem;
}
.acp-prompt-action:disabled {
  cursor: not-allowed;
  opacity: 0.45;
}

/* Empty state — a dashed card, not a bare void. */
.acp-empty {
  margin-top: 0.75rem;
  max-width: 46rem;
  border: 1px dashed var(--line);
  border-radius: 0.375rem;
  padding: 1.5rem 1.25rem;
  text-align: center;
}
.acp-empty-lede {
  font-family: var(--font-serif);
  font-size: 0.9375rem;
  color: var(--muted);
}
.acp-empty-hint {
  margin-top: 0.35rem;
  font-family: var(--font-sans);
  font-size: 0.75rem;
  color: var(--faint);
}

/* Fold heads — one quiet mono voice for thinking + activity. */
.acp-thought,
.acp-activity {
  margin-top: 0.5rem;
  max-width: 46rem;
}
.acp-fold-head {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  min-width: 0;
  max-width: 100%;
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
  flex: none;
  transition: transform 0.12s ease;
}
.chev.open {
  transform: rotate(90deg);
}
.chev.sm {
  margin-left: auto;
  color: var(--faint);
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

/* A streaming thought shows only its live tail, faded in at the top. */
.acp-thought-live-clip {
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  max-height: 7.5rem;
  overflow: hidden;
  mask-image: linear-gradient(to bottom, transparent 0, black 2.25rem);
  -webkit-mask-image: linear-gradient(to bottom, transparent 0, black 2.25rem);
}

/* Activity group — a run of tool calls folded to one line. */
.acp-activity-solo {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  min-width: 0;
}
.acp-activity-failbadge {
  flex: none;
  color: var(--block);
}
.acp-activity-list {
  margin: 0.3rem 0 0 1.15rem;
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  border-left: 1px solid var(--line);
  padding-left: 0.6rem;
}
.acp-activity-line {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  width: 100%;
  min-width: 0;
  padding: 0.1rem 0.25rem;
  border-radius: 0.2rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--muted);
  text-align: left;
}
.acp-activity-line:not(:disabled) {
  cursor: pointer;
}
.acp-activity-line:not(:disabled):hover {
  background: color-mix(in srgb, var(--subtle) 55%, transparent);
  color: var(--fg);
}
.acp-activity-status {
  margin-left: auto;
  flex: none;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
}
.acp-tool-glyph {
  flex: none;
  color: var(--muted);
}

/* Expanded call detail — recessed mono, clamped so output can't swallow the page. */
.acp-detail {
  margin: 0.2rem 0 0.35rem 0.25rem;
  border-radius: 0.25rem;
  overflow: hidden;
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
  overflow: auto;
  max-height: 16rem;
}
.acp-detail > * + * {
  border-top: 1px solid var(--line);
}

/* Diff — ±lines on the recessed tone. */
.acp-diff {
  margin: 0;
  padding: 0.4rem 0;
  background: var(--code);
  overflow: auto;
  max-height: 16rem;
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
  max-width: 46rem;
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
  margin: 1rem 0 0.25rem;
  max-width: 46rem;
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

/* Live status — the one animated element: a soft shimmer naming the agent's
   current activity while a turn runs. */
.acp-status {
  margin-top: 0.75rem;
  display: flex;
  align-items: baseline;
  gap: 0.6rem;
  font-size: 0.8125rem;
}
.acp-live-label {
  font-family: var(--font-sans);
  font-size: 0.8125rem;
  color: var(--muted);
  background: linear-gradient(90deg, var(--muted) 30%, var(--fg) 50%, var(--muted) 70%);
  background-size: 200% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: acp-shimmer 2.2s linear infinite;
}
@keyframes acp-shimmer {
  from {
    background-position: 200% 0;
  }
  to {
    background-position: -200% 0;
  }
}
@media (prefers-reduced-motion: reduce) {
  .acp-live-label {
    animation: none;
    background: none;
    -webkit-text-fill-color: currentColor;
    color: var(--muted);
  }
}
.acp-status-meta {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  color: var(--faint);
  font-variant-numeric: tabular-nums;
}

/* Composer. */
.acp-composer {
  margin-top: 0.5rem;
  border-top: 1px solid var(--line);
  padding-top: 0.625rem;
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
  flex-wrap: wrap;
  align-items: center;
  gap: 0.65rem;
  min-width: 0;
}

/* Slash-command autocomplete, populated by ACP `available_commands_update`. */
.acp-command-menu {
  max-height: 16rem;
  overflow: auto;
  border: 1px solid var(--line);
  border-top: 0;
  border-radius: 0 0 0.375rem 0.375rem;
  background: var(--surface);
  box-shadow: 0 8px 20px rgb(0 0 0 / 0.12);
  padding: 0.2rem;
}
.acp-command-item {
  display: grid;
  width: 100%;
  grid-template-columns: minmax(8rem, auto) minmax(10rem, 1fr) auto;
  align-items: baseline;
  gap: 0.75rem;
  border-radius: 0.25rem;
  padding: 0.4rem 0.55rem;
  text-align: left;
  color: var(--muted);
}
.acp-command-item[data-active='true'],
.acp-command-item:hover {
  background: var(--subtle);
  color: var(--fg);
}
.acp-command-item code,
.acp-command-item kbd {
  font-family: var(--font-mono);
  font-size: 0.75rem;
}
.acp-command-item code {
  color: var(--accent);
}
.acp-command-item kbd {
  color: var(--faint);
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
.acp-config-name {
  color: var(--faint);
}
.acp-config-timing {
  color: var(--attn);
  font-size: 0.625rem;
}
.acp-config-permission {
  border-color: color-mix(in srgb, var(--attn) 40%, var(--line));
}
.acp-config-danger {
  border-color: color-mix(in srgb, var(--block) 55%, var(--line));
  color: var(--block);
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
  cursor: pointer;
}
.acp-slash-hint:hover {
  color: var(--fg);
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
