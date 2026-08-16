<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue';
import * as api from '../api';
import { me } from '../auth';
import { openTopic, type TopicHandle } from '../lib/eventStream';
import type { Diagnostics, LogLine, ServerStatus, TaskRecord } from '../types';

// Live server logs, straight from the process's tracing output — the same lines
// that go to stdout / `docker compose logs`, but readable from the browser so an
// operator can debug a Docker deploy (a failed session recovery, a webhook that
// got rejected) without shelling into the container. Snapshot on open, then a
// live SSE tail. Admins receive the raw operator log; the server redacts known
// and credential-shaped secrets before returning these lines to user roles.

// Newest lines live at the end. Cap the client-side list so a long-lived panel
// can't grow without bound; the server ring buffer holds ~2000 anyway.
const MAX_LINES = 5000;
const lines = ref<LogLine[]>([]);
const status = ref<ServerStatus | null>(null);
const diagnostics = ref<Diagnostics | null>(null);
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
let source: TopicHandle | null = null;

function push(line: LogLine) {
  lines.value.push(line);
  if (lines.value.length > MAX_LINES) {
    lines.value.splice(0, lines.value.length - MAX_LINES);
  }
  if (pinned.value) scrollToBottom();
}

function openStream() {
  closeStream();
  source = openTopic('logs');
  // The shared stream owns reconnection; a dropped tail resumes from the ring
  // buffer's newest lines, so no snapshot is refetched here.
  source.on('log', (data) => push(data as LogLine));
}

function closeStream() {
  source?.close();
  source = null;
}

async function loadSnapshot() {
  try {
    const [snap, st, diag] = await Promise.all([
      api.getLogs(2000),
      api.getServerStatus(),
      // Keep the log and status snapshot useful when diagnostics are down.
      api.getDiagnostics().catch(() => null),
    ]);
    lines.value = snap;
    status.value = st;
    diagnostics.value = diag;
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

const shortIdentity = (value: string): string => {
  if (value === 'unknown') return value;
  const digest = value.includes('@') ? value.slice(value.lastIndexOf('@') + 1) : value;
  if (digest.startsWith('sha256:')) return `${digest.slice(0, 19)}…`;
  return digest.slice(0, 12);
};

const currentSessionCount = computed(
  () =>
    diagnostics.value?.sessions
      .filter((row) => row.status !== 'archived')
      .reduce((total, row) => total + row.count, 0) ?? 0,
);

// --- Background tasks ------------------------------------------------------
// The detached `@loom` webhook launches (clone → create → reply) that run off the
// webhook request. Polled — low-frequency, so a few-second refresh is plenty; no
// SSE like the logs.
const tasks = ref<TaskRecord[]>([]);
async function loadTasks() {
  try {
    tasks.value = await api.getTasks();
  } catch {
    /* keep the last snapshot; the log pane surfaces server-side errors */
  }
}
const taskStateClass = (s: string): string =>
  s === 'done' ? 'text-ok' : s === 'error' ? 'text-block' : 'text-info';
let taskTimer: ReturnType<typeof setInterval> | null = null;

// Refresh both the log snapshot and the task list (the toolbar Refresh button).
function refresh() {
  loadSnapshot();
  loadTasks();
}

onMounted(() => {
  loadSnapshot().then(() => {
    if (live.value) openStream();
  });
  loadTasks();
  taskTimer = setInterval(loadTasks, 5000);
});
onUnmounted(() => {
  closeStream();
  if (taskTimer) clearInterval(taskTimer);
});
</script>

<template>
  <div>
    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <!-- Build + process identity, so a redeploy is visible and attributable. -->
    <div
      v-if="status"
      class="mb-4 flex flex-wrap items-center gap-x-4 gap-y-1 rounded-md border border-line bg-surface px-3 py-2 font-mono text-2xs text-muted"
    >
      <span
        >v<span class="text-accent">{{ status.version }}</span></span
      >
      <span :title="status.build_revision">
        rev <span class="text-accent">{{ shortIdentity(status.build_revision) }}</span>
      </span>
      <span>
        profile <span class="text-accent">{{ status.build_profile }}</span>
      </span>
      <span v-if="status.image" :title="status.image">
        image <span class="text-accent">{{ shortIdentity(status.image) }}</span>
      </span>
      <span
        >pid <span class="text-accent">{{ status.pid }}</span></span
      >
      <span
        >up <span class="text-accent">{{ uptime }}</span></span
      >
      <span :title="status.started_at">started {{ shortTime(status.started_at) }}</span>
    </div>

    <!-- Durable operational state. The backend returns aggregates and mapping
         metadata only: no session ids, paths, users, tokens, or raw failures. -->
    <section v-if="diagnostics" class="mb-5 space-y-3" data-testid="diagnostics-overview">
      <div class="flex flex-wrap items-center gap-2">
        <h2 class="mr-1 text-2xs font-semibold uppercase tracking-wider text-muted">
          Control plane
        </h2>
        <span class="rounded bg-input px-2 py-1 font-mono text-2xs text-fg">
          {{ currentSessionCount }} non-archived sessions
        </span>
        <span
          v-for="migration in diagnostics.migrations"
          :key="migration.stream"
          class="rounded px-2 py-1 font-mono text-2xs"
          :class="migration.ready ? 'bg-ok/10 text-ok' : 'bg-block/10 text-block'"
        >
          {{ migration.stream }} schema {{ migration.current }}/{{ migration.expected }}
        </span>
      </div>

      <div class="grid gap-3 xl:grid-cols-2">
        <div class="overflow-x-auto rounded-md border border-line">
          <table class="w-full border-collapse font-mono text-2xs">
            <thead>
              <tr class="bg-surface text-left text-faint">
                <th class="px-2 py-1 font-medium">Profile</th>
                <th class="px-2 py-1 font-medium">Revision</th>
                <th class="px-2 py-1 font-medium">Used</th>
                <th class="px-2 py-1 font-medium">Available</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="profile in diagnostics.profiles"
                :key="profile.profile"
                class="border-t border-line/40"
              >
                <td class="px-2 py-1 text-fg">{{ profile.profile }}</td>
                <td class="px-2 py-1 text-muted">{{ profile.revision }}</td>
                <td class="px-2 py-1 text-muted">
                  {{ profile.active }} / {{ profile.maximum ?? '∞' }}
                </td>
                <td class="px-2 py-1" :class="profile.available === 0 ? 'text-block' : 'text-ok'">
                  {{ profile.available ?? 'unlimited' }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="overflow-x-auto rounded-md border border-line">
          <table class="w-full border-collapse font-mono text-2xs">
            <thead>
              <tr class="bg-surface text-left text-faint">
                <th class="px-2 py-1 font-medium">Status</th>
                <th class="px-2 py-1 font-medium">Profile</th>
                <th class="px-2 py-1 font-medium">Class</th>
                <th class="px-2 py-1 font-medium">Protocol</th>
                <th class="px-2 py-1 text-right font-medium">Count</th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="!diagnostics.sessions.length">
                <td colspan="5" class="px-2 py-2 text-muted">No sessions.</td>
              </tr>
              <tr
                v-for="row in diagnostics.sessions"
                :key="`${row.status}:${row.profile}:${row.class}:${row.protocol}:${row.runner_pool}`"
                class="border-t border-line/40"
              >
                <td
                  class="px-2 py-1"
                  :class="
                    row.status === 'error'
                      ? 'text-block'
                      : row.status === 'orphaned'
                        ? 'text-attn'
                        : 'text-fg'
                  "
                >
                  {{ row.status }}
                </td>
                <td class="px-2 py-1 text-muted">{{ row.profile }}</td>
                <td class="px-2 py-1 text-muted">{{ row.class }}</td>
                <td class="px-2 py-1 text-muted">{{ row.protocol }}</td>
                <td class="px-2 py-1 text-right text-fg">{{ row.count }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <div class="grid gap-3 xl:grid-cols-2">
        <div class="rounded-md border border-line bg-surface px-3 py-2 text-xs">
          <h3 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-muted">
            Automation runs
          </h3>
          <p v-if="!diagnostics.automation_runs.counts.length" class="text-faint">No runs yet.</p>
          <div v-else class="flex flex-wrap gap-1.5 font-mono text-2xs">
            <span
              v-for="run in diagnostics.automation_runs.counts"
              :key="`${run.status}:${run.source}:${run.service_tag}:${run.profile}`"
              class="rounded bg-input px-2 py-1"
              :class="run.status === 'failed' ? 'text-block' : 'text-muted'"
            >
              {{ run.service_tag }}/{{ run.profile }} · {{ run.status }} {{ run.count }}
            </span>
          </div>
          <p v-if="diagnostics.automation_runs.stale_creating" class="mt-2 text-2xs text-block">
            {{ diagnostics.automation_runs.stale_creating }} creating for more than five minutes
          </p>
        </div>

        <div class="rounded-md border border-line bg-surface px-3 py-2 text-xs">
          <h3 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-muted">
            Orphan / error inventory
          </h3>
          <p v-if="!diagnostics.problems.length" class="text-ok">No orphaned or error sessions.</p>
          <div v-else class="space-y-1 font-mono text-2xs">
            <p
              v-for="problem in diagnostics.problems"
              :key="`${problem.status}:${problem.profile}:${problem.protocol}`"
            >
              <span :class="problem.status === 'error' ? 'text-block' : 'text-attn'">
                {{ problem.count }} {{ problem.status }}
              </span>
              <span class="text-muted">
                · {{ problem.profile }} / {{ problem.class }} / {{ problem.protocol }} /
                {{ problem.runner_pool }}
              </span>
              <span v-if="problem.latest_activity_at" class="text-faint">
                · latest {{ shortTime(problem.latest_activity_at) }}
              </span>
            </p>
          </div>
        </div>
      </div>

      <details
        v-if="diagnostics.federations.length"
        class="rounded-md border border-line bg-surface px-3 py-2"
      >
        <summary class="cursor-pointer text-2xs font-semibold uppercase tracking-wider text-muted">
          Federation mappings ({{ diagnostics.federations.length }})
        </summary>
        <div class="mt-2 overflow-x-auto">
          <table class="w-full border-collapse font-mono text-2xs">
            <thead>
              <tr class="text-left text-faint">
                <th class="px-2 py-1 font-medium">Name</th>
                <th class="px-2 py-1 font-medium">Provider / service</th>
                <th class="px-2 py-1 font-medium">Audience</th>
                <th class="px-2 py-1 font-medium">Profiles</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="mapping in diagnostics.federations"
                :key="mapping.name"
                class="border-t border-line/40"
              >
                <td class="px-2 py-1 text-fg">{{ mapping.name }}</td>
                <td class="px-2 py-1 text-muted">
                  {{ mapping.provider }} · {{ mapping.service_tag }}
                </td>
                <td class="px-2 py-1 break-all text-muted">{{ mapping.audience }}</td>
                <td class="px-2 py-1 text-muted">{{ mapping.profiles.join(', ') }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </details>
    </section>

    <!-- Background tasks: the detached @loom webhook launches. -->
    <section class="mb-5">
      <h2 class="mb-1.5 text-2xs font-semibold uppercase tracking-wider text-muted">
        Background tasks
      </h2>
      <p class="mb-2 text-xs text-faint">
        Detached <code>@loom</code> webhook launches — the clone, session create, and reply that run
        after the webhook returns its <code>200</code>. Newest first.
      </p>
      <div class="overflow-x-auto rounded-md border border-line">
        <table class="w-full border-collapse font-mono text-2xs">
          <thead>
            <tr class="bg-surface text-left text-faint">
              <th class="px-2 py-1 font-medium">State</th>
              <th class="px-2 py-1 font-medium">Kind</th>
              <th class="px-2 py-1 font-medium">Task</th>
              <th class="px-2 py-1 font-medium">Detail</th>
              <th class="whitespace-nowrap px-2 py-1 font-medium">Started</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="!tasks.length">
              <td colspan="5" class="px-2 py-2 text-muted">No background tasks yet.</td>
            </tr>
            <tr v-for="t in tasks" :key="t.id" class="border-t border-line/40 align-top">
              <td class="px-2 py-1 font-semibold" :class="taskStateClass(t.state)">
                {{ t.state }}
              </td>
              <td class="whitespace-nowrap px-2 py-1 text-muted">{{ t.kind }}</td>
              <td class="px-2 py-1 break-all">{{ t.label }}</td>
              <td class="px-2 py-1 break-all text-muted">{{ t.detail || '—' }}</td>
              <td class="whitespace-nowrap px-2 py-1 text-faint" :title="t.started_at">
                {{ shortTime(t.started_at) }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <!-- Server logs -->
    <div class="mb-1.5 flex items-center gap-2">
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Server logs</h2>
      <span v-if="me.role === 'user'" class="pill">secrets redacted</span>
    </div>
    <p class="mb-3 text-xs text-faint">
      The running server's log stream, live. The same lines go to
      <code>docker compose logs</code>; this is a read-only mirror so you can debug from the
      browser.
      <template v-if="me.role === 'user'">
        Known deployment credentials and token-shaped values are redacted.
      </template>
      <template v-else> Administrators see the raw operator stream. </template>
    </p>

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

      <button class="btn-secondary px-2.5 py-1 text-xs" @click="refresh">Refresh</button>
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
