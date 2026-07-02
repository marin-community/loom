<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue';
import * as api from '../api';
import type { LogLine, ServerStatus } from '../types';

// Live server logs, straight from the process's tracing output — the same lines
// that go to stdout / `docker compose logs`, but readable from the browser so an
// operator can debug a Docker deploy (a failed session recovery, a webhook that
// got rejected) without shelling into the container. Snapshot on open, then a
// live SSE tail. Operator-only on the server; server logs can carry secrets.

// Newest lines live at the end. Cap the client-side list so a long-lived panel
// can't grow without bound; the server ring buffer holds ~2000 anyway.
const MAX_LINES = 5000;
const lines = ref<LogLine[]>([]);
const status = ref<ServerStatus | null>(null);
const error = ref('');

// Filters. `minLevel` is a severity floor (show this and louder).
const LEVELS = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR'] as const;
const rank = (l: string) => {
  const i = LEVELS.indexOf(l.toUpperCase() as (typeof LEVELS)[number]);
  return i < 0 ? LEVELS.indexOf('INFO') : i;
};
const minLevel = ref<string>('INFO');
const query = ref('');

const filtered = computed(() => {
  const floor = rank(minLevel.value);
  const q = query.value.trim().toLowerCase();
  return lines.value.filter((l) => {
    if (rank(l.level) < floor) return false;
    if (!q) return true;
    return (
      l.message.toLowerCase().includes(q) ||
      l.target.toLowerCase().includes(q) ||
      l.level.toLowerCase().includes(q)
    );
  });
});

const levelClass = (l: string): string => {
  switch (l.toUpperCase()) {
    case 'ERROR':
      return 'text-block';
    case 'WARN':
      return 'text-attn';
    case 'INFO':
      return 'text-info';
    case 'DEBUG':
      return 'text-muted';
    default:
      return 'text-faint';
  }
};

// Compact HH:MM:SS.mmm for the row; full timestamp on hover.
const shortTime = (ts: string): string => {
  const d = new Date(ts);
  if (isNaN(d.getTime())) return ts;
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
};

// --- Live stream -----------------------------------------------------------
const live = ref(true);
let source: EventSource | null = null;

function push(line: LogLine) {
  lines.value.push(line);
  if (lines.value.length > MAX_LINES) {
    lines.value.splice(0, lines.value.length - MAX_LINES);
  }
  if (pinned.value) scrollToBottom();
}

function openStream() {
  closeStream();
  source = new EventSource('/api/logs/stream');
  source.addEventListener('log', (e) => {
    try {
      push(JSON.parse((e as MessageEvent).data) as LogLine);
    } catch {
      /* ignore a malformed frame */
    }
  });
  // EventSource auto-reconnects on transient errors; nothing to do here.
}

function closeStream() {
  source?.close();
  source = null;
}

async function loadSnapshot() {
  try {
    const [snap, st] = await Promise.all([api.getLogs(2000), api.getServerStatus()]);
    lines.value = snap;
    status.value = st;
    error.value = '';
    await nextTick();
    scrollToBottom(true);
  } catch (e) {
    error.value = (e as Error).message;
  }
}

// Toggling Live re-snapshots (to fill any gap) and (re)opens the stream, or
// tears it down. Snapshot-then-stream can double-count the boundary line, but
// the seq dedupe below drops it.
watch(live, (on) => {
  if (on) {
    loadSnapshot().then(() => openStream());
  } else {
    closeStream();
  }
});

// --- Autoscroll ------------------------------------------------------------
const pane = ref<HTMLElement | null>(null);
const pinned = ref(true); // stay stuck to the bottom until the user scrolls up

function scrollToBottom(force = false) {
  const el = pane.value;
  if (!el) return;
  if (force) pinned.value = true;
  if (pinned.value) nextTick(() => (el.scrollTop = el.scrollHeight));
}

function onScroll() {
  const el = pane.value;
  if (!el) return;
  // A small slack so sub-pixel rounding doesn't unpin us.
  pinned.value = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
}

// --- Actions ---------------------------------------------------------------
const asText = () =>
  filtered.value.map((l) => `${l.ts} ${l.level.padEnd(5)} ${l.target}: ${l.message}`).join('\n');

const copied = ref(false);
async function copyLogs() {
  try {
    await navigator.clipboard.writeText(asText());
    copied.value = true;
    setTimeout(() => (copied.value = false), 1500);
  } catch (e) {
    error.value = (e as Error).message;
  }
}

function downloadLogs() {
  const blob = new Blob([asText()], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `loom-logs-${new Date().toISOString().replace(/[:.]/g, '-')}.txt`;
  a.click();
  URL.revokeObjectURL(url);
}

function clearView() {
  lines.value = [];
}

// De-dupe the seq numbers already onboard so the snapshot/stream boundary and
// any EventSource replay don't render the same line twice.
const deduped = computed(() => {
  const seen = new Set<number>();
  return filtered.value.filter((l) => {
    if (seen.has(l.seq)) return false;
    seen.add(l.seq);
    return true;
  });
});

const uptime = computed(() => {
  if (!status.value) return '';
  const started = new Date(status.value.started_at).getTime();
  if (isNaN(started)) return '';
  const secs = Math.max(0, Math.floor((Date.now() - started) / 1000));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${s}s` : `${s}s`;
});

onMounted(() => {
  loadSnapshot().then(() => {
    if (live.value) openStream();
  });
});
onUnmounted(closeStream);
</script>

<template>
  <div>
    <h2 class="mb-1.5 text-2xs font-semibold uppercase tracking-wider text-muted">Server logs</h2>
    <p class="mb-3 text-xs text-faint">
      The running server's log stream, live. The same lines go to
      <code>docker compose logs</code>; this is a read-only mirror so you can debug from the browser.
      May contain secrets — visible to approved operators only.
    </p>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <!-- Status line -->
    <div
      v-if="status"
      class="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1 rounded-md border border-line bg-surface px-3 py-2 font-mono text-2xs text-muted"
    >
      <span>v<span class="text-accent">{{ status.version }}</span></span>
      <span>pid <span class="text-accent">{{ status.pid }}</span></span>
      <span>up <span class="text-accent">{{ uptime }}</span></span>
      <span :title="status.started_at">started {{ shortTime(status.started_at) }}</span>
    </div>

    <!-- Controls -->
    <div class="mb-2 flex flex-wrap items-center gap-2">
      <button
        class="px-2.5 py-1 text-xs"
        :class="live ? 'btn-primary' : 'btn-secondary'"
        @click="live = !live"
      >
        {{ live ? '● Live' : '▶ Paused' }}
      </button>

      <label class="flex items-center gap-1 text-2xs text-muted">
        Level
        <select
          v-model="minLevel"
          class="rounded bg-input px-1.5 py-1 text-xs outline-none focus:ring-1 ring-accent"
        >
          <option v-for="l in [...LEVELS].reverse()" :key="l" :value="l">{{ l }}+</option>
        </select>
      </label>

      <input
        v-model="query"
        placeholder="filter…"
        spellcheck="false"
        class="min-w-0 flex-1 rounded bg-input px-2 py-1 font-mono text-xs outline-none focus:ring-1 ring-accent"
      />

      <button class="btn-secondary px-2.5 py-1 text-xs" @click="loadSnapshot">Refresh</button>
      <button class="btn-secondary px-2.5 py-1 text-xs" @click="copyLogs">
        {{ copied ? 'Copied' : 'Copy' }}
      </button>
      <button class="btn-secondary px-2.5 py-1 text-xs" @click="downloadLogs">Download</button>
      <button class="btn-secondary px-2.5 py-1 text-xs" @click="clearView">Clear</button>
    </div>

    <!-- Log pane -->
    <div
      ref="pane"
      class="h-[28rem] overflow-auto rounded-md border border-line bg-canvas p-2 font-mono text-2xs leading-relaxed"
      @scroll="onScroll"
    >
      <p v-if="!deduped.length" class="p-2 text-muted">
        No matching log lines{{ lines.length ? ' (adjust the filter)' : ' yet' }}.
      </p>
      <div
        v-for="l in deduped"
        :key="l.seq"
        class="flex gap-2 whitespace-pre-wrap break-words border-b border-line/40 py-0.5 last:border-0"
      >
        <span class="shrink-0 text-faint" :title="l.ts">{{ shortTime(l.ts) }}</span>
        <span class="w-10 shrink-0 font-semibold" :class="levelClass(l.level)">{{ l.level }}</span>
        <span class="shrink-0 text-faint">{{ l.target }}</span>
        <span class="min-w-0">{{ l.message }}</span>
      </div>
    </div>
    <p class="mt-1 text-2xs text-faint">
      Showing {{ deduped.length }} of {{ lines.length }} line{{ lines.length === 1 ? '' : 's' }}
      <span v-if="!pinned"> · scroll to bottom to follow live</span>
    </p>
  </div>
</template>
