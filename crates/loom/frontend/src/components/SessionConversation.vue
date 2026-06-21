<script setup lang="ts">
import { ref, reactive, computed, onMounted, watch, nextTick } from 'vue';
import { get } from '../api';
import type { IrisLog, IrisBlock } from '../types';
import MarkdownView from './MarkdownView.vue';

// The Conversation tab: the agent's chat with the model, rendered for review.
// Reads the normalized iris log from `GET /sessions/{id}/conversation` (live
// transcript when present, else the capture archived on teardown) and renders it
// as a *skimmable* log — prose stays in full view, while the agent's machinery
// (tool calls + outputs, thinking, injected context) folds away behind compact
// one-liners. Runs of the same tool call are run-length collapsed (`10× TaskCreate`)
// so a burst of bookkeeping never buries the conversation, and a right-hand
// prompt index lets a reviewer jump straight to any user turn.
const props = defineProps<{ id: string }>();

type LoadState = 'loading' | 'ready' | 'empty' | 'error';
const log = ref<IrisLog | null>(null);
const state = ref<LoadState>('loading');
const errorMsg = ref('');

async function load() {
  state.value = 'loading';
  try {
    const data = (await get(`/sessions/${props.id}/conversation`)) as IrisLog;
    log.value = data;
    state.value = data && data.messages.length ? 'ready' : 'empty';
  } catch (e) {
    // A 404 means nothing's been recorded (a shell session, or not yet) — that's
    // an empty state, not an error worth shouting about.
    const msg = (e as Error).message ?? '';
    if (/not found|conversation/i.test(msg)) {
      state.value = 'empty';
    } else {
      errorMsg.value = msg;
      state.value = 'error';
    }
  }
  await nextTick();
  updateActive();
}

onMounted(load);
watch(() => props.id, load);

// ── The render model ────────────────────────────────────────────────────────
// We flatten the message/block stream into a flat list of `Row`s the template
// renders directly. Two transforms happen here: tool_use blocks are paired with
// the tool_result that follows them, and consecutive tool items sharing a name
// are run-length grouped. A `TocItem` is emitted for every user turn, anchoring
// the jump list on the right.

interface ToolItem {
  name: string;
  input: unknown;
  result?: { output: string; is_error: boolean };
}
/** A run-length group of consecutive same-name tool items (`items.length >= 1`). */
interface ToolGroup {
  name: string;
  items: ToolItem[];
}
type Row =
  | { type: 'context'; key: number; text: string }
  | { type: 'user'; key: number; anchor: string; n: number; time?: string; blocks: IrisBlock[] }
  | { type: 'text'; key: number; text: string }
  | { type: 'thinking'; key: number; text: string }
  | { type: 'tools'; key: number; groups: ToolGroup[] }
  | { type: 'image'; key: number };
interface TocItem {
  anchor: string;
  n: number;
  title: string;
}
interface Model {
  rows: Row[];
  toc: TocItem[];
  foldKeys: string[];
  counts: { tools: number; thinking: number; context: number };
}

/** Run-length encode a flat tool stream: fold a run of same-name items into one group. */
function rle(items: ToolItem[]): ToolGroup[] {
  const groups: ToolGroup[] = [];
  for (const it of items) {
    const last = groups[groups.length - 1];
    if (last && last.name === it.name) last.items.push(it);
    else groups.push({ name: it.name, items: [it] });
  }
  return groups;
}

/** The first non-blank line of a string, trimmed (`''` if there is none). */
function firstLine(s: string): string {
  return s.split('\n').map((l) => l.trim()).find(Boolean) ?? '';
}

/** A user prompt's jump-list label: the first non-empty line, lightly de-marked. */
function userTitle(blocks: IrisBlock[]): string {
  const t = blocks.find((b) => b.kind === 'text') as { text: string } | undefined;
  const first = firstLine(t?.text ?? '') || '(no text)';
  return first.replace(/^[#>\-*`\s]+/, '').trim() || first;
}

const model = computed<Model>(() => {
  const rows: Row[] = [];
  const toc: TocItem[] = [];
  const foldKeys: string[] = [];
  const counts = { tools: 0, thinking: 0, context: 0 };
  let key = 0;
  let turn = 0;
  let toolBuf: ToolItem[] = [];

  const flushTools = () => {
    if (!toolBuf.length) return;
    const k = key++;
    const groups = rle(toolBuf);
    rows.push({ type: 'tools', key: k, groups });
    groups.forEach((_, gi) => foldKeys.push(`tg-${k}-${gi}`));
    toolBuf = [];
  };

  for (const msg of log.value?.messages ?? []) {
    if (msg.role === 'context') {
      flushTools();
      const text = msg.blocks
        .filter((b): b is { kind: 'text' | 'thinking'; text: string } => b.kind === 'text' || b.kind === 'thinking')
        .map((b) => b.text)
        .join('\n\n')
        .trim();
      if (text) {
        const k = key++;
        rows.push({ type: 'context', key: k, text });
        foldKeys.push(`ctx-${k}`);
        counts.context++;
      }
      continue;
    }
    if (msg.role === 'user') {
      flushTools();
      turn++;
      const anchor = `conv-turn-${turn}`;
      rows.push({ type: 'user', key: key++, anchor, n: turn, time: msg.timestamp, blocks: msg.blocks });
      toc.push({ anchor, n: turn, title: userTitle(msg.blocks) });
      continue;
    }
    // assistant
    for (const b of msg.blocks) {
      switch (b.kind) {
        case 'text':
          flushTools();
          if (b.text.trim()) rows.push({ type: 'text', key: key++, text: b.text });
          break;
        case 'thinking': {
          flushTools();
          if (b.text.trim()) {
            const k = key++;
            rows.push({ type: 'thinking', key: k, text: b.text });
            foldKeys.push(`think-${k}`);
            counts.thinking++;
          }
          break;
        }
        case 'tool_use':
          toolBuf.push({ name: b.name, input: b.input });
          counts.tools++;
          break;
        case 'tool_result': {
          const last = toolBuf[toolBuf.length - 1];
          if (last && !last.result) last.result = { output: b.output, is_error: b.is_error };
          else toolBuf.push({ name: '↳ result', input: undefined, result: { output: b.output, is_error: b.is_error } });
          break;
        }
        case 'image':
          flushTools();
          rows.push({ type: 'image', key: key++ });
          break;
      }
    }
  }
  flushTools();
  return { rows, toc, foldKeys, counts };
});

// ── Visibility filters + fold state ─────────────────────────────────────────
// The machinery is hidden by default in the sense that it renders collapsed;
// these category toggles let a reviewer remove a class of noise entirely.
const show = reactive({ tools: true, thinking: true, context: true });
const visibleRows = computed(() =>
  model.value.rows.filter((r) => {
    if (r.type === 'tools') return show.tools;
    if (r.type === 'thinking') return show.thinking;
    if (r.type === 'context') return show.context;
    return true;
  }),
);

// Which folds are open. Default: empty (everything collapsed) — that's the
// whole point: tool calls/outputs and bookkeeping stay tucked until asked for.
const open = ref<Set<string>>(new Set());
const isOpen = (k: string) => open.value.has(k);
function toggle(k: string) {
  const s = new Set(open.value);
  s.has(k) ? s.delete(k) : s.add(k);
  open.value = s;
}
const allOpen = computed(() => model.value.foldKeys.length > 0 && open.value.size >= model.value.foldKeys.length);
function toggleAll() {
  open.value = allOpen.value ? new Set() : new Set(model.value.foldKeys);
}

// ── Prompt jump-list (scroll-spy) ───────────────────────────────────────────
const convScroll = ref<HTMLElement | null>(null);
const activeAnchor = ref('');

// The "current" prompt is the last user turn scrolled to (or past) the top of
// the viewport — so the index highlight tracks the turn you're reading.
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

// Re-seed the highlight whenever the rendered turns change (load, id switch, a
// filter toggle that shifts the layout).
watch(visibleRows, () => nextTick(updateActive));

function goTo(anchor: string) {
  activeAnchor.value = anchor;
  convScroll.value
    ?.querySelector(`[data-anchor="${CSS.escape(anchor)}"]`)
    ?.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

// ── Banner + small formatters ───────────────────────────────────────────────
const banner = computed(() => {
  const l = log.value;
  if (!l) return '';
  const parts: string[] = [];
  if (l.source) parts.push(l.source);
  parts.push(`${l.messages.length} messages`);
  if (l.model) parts.push(l.model);
  const times = l.messages.map((m) => m.timestamp).filter(Boolean) as string[];
  if (times.length) parts.push(`${shortTime(times[0])} – ${shortTime(times[times.length - 1])}`);
  return parts.join(' · ');
});

function shortTime(ts?: string): string {
  if (!ts) return '';
  return ts.length >= 19 && ts[10] === 'T' ? ts.slice(11, 19) : ts;
}

// A shell command (Bash `command`, Codex `cmd`) renders as the command itself;
// any other tool input as pretty JSON or its raw string.
function toolCommand(input: unknown): string | null {
  if (input && typeof input === 'object') {
    const o = input as Record<string, unknown>;
    const c = o.command ?? o.cmd;
    if (typeof c === 'string') return c;
  }
  return null;
}
function inputText(it: ToolItem): string {
  if (it.input === undefined) return '';
  if (typeof it.input === 'string') return it.input;
  const cmd = toolCommand(it.input);
  if (cmd !== null) return cmd;
  try {
    return JSON.stringify(it.input, null, 2);
  } catch {
    return String(it.input);
  }
}
/** A one-line hint shown beside a single tool's collapsed header. */
function preview(it: ToolItem): string {
  return firstLine(inputText(it));
}
const groupHasError = (g: ToolGroup) => g.items.some((it) => it.result?.is_error);
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Toolbar: banner · category filters · expand-all · refresh. A live
         session's conversation grows, so a manual reload stays available. -->
    <div class="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
      <p class="min-w-0 flex-1 truncate text-xs text-muted">{{ banner }}</p>

      <div v-if="state === 'ready'" class="flex items-center gap-1" data-testid="conversation-filters">
        <button
          type="button"
          class="chip"
          :data-active="show.tools"
          aria-label="Toggle tool calls"
          @click="show.tools = !show.tools"
        >
          Tools<span v-if="model.counts.tools" class="chip-n">{{ model.counts.tools }}</span>
        </button>
        <button
          type="button"
          class="chip"
          :data-active="show.thinking"
          aria-label="Toggle thinking"
          @click="show.thinking = !show.thinking"
        >
          Thinking<span v-if="model.counts.thinking" class="chip-n">{{ model.counts.thinking }}</span>
        </button>
        <button
          v-if="model.counts.context"
          type="button"
          class="chip"
          :data-active="show.context"
          aria-label="Toggle context"
          @click="show.context = !show.context"
        >
          Context<span class="chip-n">{{ model.counts.context }}</span>
        </button>
        <span class="mx-0.5 h-3.5 w-px bg-line"></span>
        <button
          type="button"
          class="chip"
          :disabled="!model.foldKeys.length"
          @click="toggleAll"
        >
          {{ allOpen ? 'Collapse all' : 'Expand all' }}
        </button>
      </div>

      <button
        type="button"
        class="btn-secondary shrink-0 px-2 py-0.5 text-xs"
        :disabled="state === 'loading'"
        @click="load"
      >
        Refresh
      </button>
    </div>

    <p v-if="state === 'loading'" class="text-sm text-muted">Loading conversation…</p>
    <p v-else-if="state === 'error'" class="text-sm text-block">{{ errorMsg }}</p>
    <p v-else-if="state === 'empty'" class="text-sm text-muted">
      No conversation recorded for this session yet.
    </p>

    <div v-else class="flex min-h-0 flex-1 gap-4">
      <!-- The conversation stream. -->
      <div
        ref="convScroll"
        data-testid="conversation"
        class="conv-scroll min-h-0 flex-1 space-y-3 overflow-auto pb-8 pr-1"
        @scroll.passive="updateActive"
      >
        <template v-for="row in visibleRows" :key="row.key">
          <!-- Injected context (primers, system/permissions) — folded away. -->
          <div v-if="row.type === 'context'" class="overflow-hidden rounded border border-line bg-subtle/30">
            <button type="button" class="fold-head text-muted" @click="toggle('ctx-' + row.key)">
              <span class="chev" :class="{ open: isOpen('ctx-' + row.key) }">▸</span>
              <span>📎 Context</span>
            </button>
            <pre
              v-if="isOpen('ctx-' + row.key)"
              class="conv-pre border-t border-line text-muted"
              >{{ row.text }}</pre>
          </div>

          <!-- User turn — the anchor for the jump list; always shown in full. -->
          <section
            v-else-if="row.type === 'user'"
            :id="row.anchor"
            :data-anchor="row.anchor"
            class="conv-anchor rounded-md border-l-2 border-accent bg-subtle/40 px-3 py-2"
          >
            <header class="mb-1 flex items-center gap-2 text-xs font-medium text-accent">
              <span class="turn-badge">{{ row.n }}</span>
              <span>🧑 User</span>
              <span v-if="row.time" class="font-normal text-muted">{{ shortTime(row.time) }}</span>
            </header>
            <template v-for="(b, j) in row.blocks" :key="j">
              <MarkdownView v-if="b.kind === 'text'" :id="props.id" path="" :source="b.text" />
              <p v-else-if="b.kind === 'image'" class="text-xs italic text-muted">[image]</p>
            </template>
          </section>

          <!-- Assistant prose. No role heading — the boxed/accented user turns
               are the dividers, so the agent's replies just flow as plain text. -->
          <div v-else-if="row.type === 'text'" class="px-3">
            <MarkdownView :id="props.id" path="" :source="row.text" />
          </div>

          <!-- Thinking — folded. -->
          <div v-else-if="row.type === 'thinking'" class="overflow-hidden rounded border border-line bg-subtle/30">
            <button type="button" class="fold-head text-muted" @click="toggle('think-' + row.key)">
              <span class="chev" :class="{ open: isOpen('think-' + row.key) }">▸</span>
              <span>💭 Thinking</span>
            </button>
            <pre
              v-if="isOpen('think-' + row.key)"
              class="conv-pre border-t border-line text-muted"
              >{{ row.text }}</pre>
          </div>

          <!-- Tool activity — each run-length group a compact, collapsed strip. -->
          <div v-else-if="row.type === 'tools'" class="space-y-1">
            <div
              v-for="(g, gi) in row.groups"
              :key="gi"
              class="overflow-hidden rounded border border-line bg-subtle/30"
              data-testid="tool-fold"
            >
              <button type="button" class="fold-head" @click="toggle(`tg-${row.key}-${gi}`)">
                <span class="chev" :class="{ open: isOpen(`tg-${row.key}-${gi}`) }">▸</span>
                <span class="shrink-0">🔧</span>
                <span class="shrink-0 font-mono text-fg">{{ g.name }}</span>
                <span v-if="g.items.length > 1" class="rle-badge" data-testid="rle-count">{{ g.items.length }}×</span>
                <span v-else class="min-w-0 truncate font-mono text-faint">{{ preview(g.items[0]) }}</span>
                <span v-if="groupHasError(g)" class="ml-auto shrink-0 text-2xs font-medium text-block">error</span>
              </button>
              <div v-if="isOpen(`tg-${row.key}-${gi}`)" class="border-t border-line">
                <div
                  v-for="(it, ii) in g.items"
                  :key="ii"
                  :class="ii > 0 ? 'border-t border-line/60' : ''"
                >
                  <div v-if="g.items.length > 1" class="px-3 pt-1.5 font-mono text-2xs text-faint">
                    #{{ ii + 1 }} · {{ it.name }}
                  </div>
                  <pre v-if="inputText(it)" class="conv-pre text-fg">{{ inputText(it) }}</pre>
                  <pre
                    v-if="it.result && it.result.output.trim()"
                    class="conv-pre conv-result"
                    :class="it.result.is_error ? 'text-block' : 'text-muted'"
                    >{{ it.result.output }}</pre>
                </div>
              </div>
            </div>
          </div>

          <p v-else-if="row.type === 'image'" class="text-xs italic text-muted">[image]</p>
        </template>
      </div>

      <!-- Prompt jump-list: one entry per user turn, with scroll-spy highlight. -->
      <nav
        v-if="model.toc.length"
        class="hidden w-56 shrink-0 overflow-auto border-l border-line pl-3 lg:block"
        data-testid="conversation-toc"
        aria-label="Prompts"
      >
        <p class="mb-2 px-1 text-2xs font-medium uppercase tracking-wider text-faint">
          Prompts · {{ model.toc.length }}
        </p>
        <ul class="space-y-0.5">
          <li v-for="t in model.toc" :key="t.anchor">
            <button
              type="button"
              class="toc-item"
              :data-active="activeAnchor === t.anchor"
              data-testid="conversation-toc-item"
              @click="goTo(t.anchor)"
            >
              <span class="toc-num">{{ t.n }}</span>
              <span class="truncate">{{ t.title }}</span>
            </button>
          </li>
        </ul>
      </nav>
    </div>
  </div>
</template>

<style scoped>
/* Anchored user turns sit a touch below the top edge when jumped to. */
.conv-anchor {
  scroll-margin-top: 0.5rem;
}

/* The shared MarkdownView wraps prose in a padded, centred surface card — right
   for a standalone document, too heavy for a chat line. Inside the conversation
   we flatten it to tight, left-aligned prose directly on the canvas so the agent's
   replies read as conversation, not as a stack of cards. */
.conv-scroll :deep(div:has(> .markdown-body)) {
  background: transparent;
  overflow: visible;
}
.conv-scroll :deep(.markdown-body) {
  max-width: none;
  margin: 0;
  padding: 0.125rem 0;
}

/* The clickable header of any fold (context, thinking, a tool group). */
.fold-head {
  display: flex;
  width: 100%;
  align-items: center;
  gap: 0.5rem;
  padding: 0.3125rem 0.625rem;
  text-align: left;
  font-size: 0.75rem;
  line-height: 1rem;
  cursor: pointer;
}
.fold-head:hover {
  background: color-mix(in srgb, var(--subtle) 60%, transparent);
}

/* The disclosure caret — rotates from ▸ to ▾ when its fold opens. */
.chev {
  display: inline-block;
  flex: none;
  color: var(--faint);
  transition: transform 0.12s ease;
}
.chev.open {
  transform: rotate(90deg);
}

/* Run-length count badge (`10×`) for a folded run of identical tool calls. */
.rle-badge {
  flex: none;
  border-radius: 0.25rem;
  background: var(--input);
  color: var(--muted);
  box-shadow: inset 0 0 0 1px var(--line);
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  line-height: 1rem;
  padding: 0 0.375rem;
}

/* Turn number on a user prompt's header. */
.turn-badge {
  display: inline-flex;
  min-width: 1.25rem;
  justify-content: center;
  border-radius: 0.25rem;
  background: var(--input);
  color: var(--muted);
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  line-height: 1rem;
  padding: 0 0.25rem;
}

/* Preformatted bodies inside folds — tool input, results, context, thinking. */
.conv-pre {
  margin: 0;
  overflow-x: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.1rem;
  padding: 0.5rem 0.625rem;
}
.conv-result {
  max-height: 22rem;
  overflow: auto;
  background: color-mix(in srgb, var(--subtle) 35%, transparent);
}

/* Filter / expand-all chips in the toolbar. */
.chip {
  border-radius: 0.25rem;
  padding: 0.125rem 0.5rem;
  font-size: 0.6875rem;
  line-height: 1rem;
  font-weight: 500;
  color: var(--faint);
  cursor: pointer;
  transition: color 0.12s ease, background-color 0.12s ease;
  display: inline-flex;
  align-items: center;
  gap: 0.3125rem;
}
.chip:hover:not(:disabled) {
  color: var(--muted);
  background: color-mix(in srgb, var(--subtle) 50%, transparent);
}
.chip[data-active='true'] {
  color: var(--fg);
  background: var(--subtle);
}
.chip:disabled {
  opacity: 0.45;
  cursor: default;
}
.chip-n {
  font-family: var(--font-mono);
  font-size: 0.625rem;
  color: var(--faint);
}

/* Jump-list entries. */
.toc-item {
  display: flex;
  width: 100%;
  align-items: baseline;
  gap: 0.5rem;
  border-radius: 0.25rem;
  border-left: 2px solid transparent;
  padding: 0.1875rem 0.5rem;
  text-align: left;
  font-size: 0.75rem;
  line-height: 1.1rem;
  color: var(--muted);
  cursor: pointer;
  transition: color 0.12s ease, background-color 0.12s ease;
}
.toc-item:hover {
  background: color-mix(in srgb, var(--subtle) 55%, transparent);
  color: var(--fg);
}
.toc-item[data-active='true'] {
  background: var(--subtle);
  border-left-color: var(--accent);
  color: var(--fg);
}
.toc-num {
  flex: none;
  font-family: var(--font-mono);
  font-size: 0.625rem;
  color: var(--faint);
}
</style>
