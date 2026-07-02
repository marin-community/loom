<script setup lang="ts">
import { ref, reactive, computed, watch as watchEffect, onMounted, onActivated } from 'vue';
import { useRouter } from 'vue-router';
import { get, post, patch, del } from '../api';
import type { Watch, WatchRun, WatchRunResult, ProgramView } from '../types';
import OutcomeBadge from '../components/OutcomeBadge.vue';
import AgentTerminal from '../components/AgentTerminal.vue';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import { timeAgo } from '../lib/time';
import {
  triggerSummary,
  scopeSummary,
  repoLabel,
  promptOf,
  capabilitiesFrom,
  GRANTABLE_CAPABILITIES,
} from '../lib/watch';

// Named so App.vue's <keep-alive :include> keeps this view warm across nav.
defineOptions({ name: 'Watches' });

// The Watches panel — a master–detail workbench over the fleet's watch
// programs. The left column lists every watch (builtins are seeded by the
// daemon, so each appears exactly once, active or not); the right pane shows
// the selected watch's activity log, script source, and config. API-first:
// every row is a `WatchView`, every control a REST call.
const props = defineProps<{ id?: string }>();
const router = useRouter();

const watches = ref<Watch[]>([]);
const programs = ref<ProgramView[]>([]);
const loaded = ref(false);
const error = ref('');
const busy = ref(false);

// ── Selection ───────────────────────────────────────────────────────────────
// The selected watch drives the right pane. The route (`/watches/:id`) is the
// source of truth so a watch is deep-linkable; clicking a row navigates.
const selectedId = ref('');
const selected = computed(() => watches.value.find((w) => w.id === selectedId.value) ?? null);

// The right pane's mode: a selected watch, or the create form.
const creatingNew = ref(false);

function select(w: Watch) {
  creatingNew.value = false;
  if (w.id !== selectedId.value) router.push(`/watches/${w.id}`);
  selectedId.value = w.id;
}

// A builtin watch is the daemon-seeded row for a stock program (its name is
// the program's short name). It can be disabled but not deleted — the daemon
// re-seeds missing builtins on boot.
function isBuiltin(w: Watch): boolean {
  return w.program === `builtin:${w.name}`;
}

const programInfo = computed(
  () => programs.value.find((p) => p.program === selected.value?.program) ?? null,
);

const activeCount = computed(() => watches.value.filter((w) => w.enabled).length);

// ── Loading ─────────────────────────────────────────────────────────────────
async function load() {
  try {
    watches.value = (await get('/watches')) as Watch[];
    error.value = '';
    // Adopt the route's id, else fall back to the first watch so the pane is
    // never pointlessly empty.
    if (!selected.value && !creatingNew.value && watches.value.length) {
      selectedId.value = props.id || watches.value[0].id;
    }
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loaded.value = true;
  }
}

async function loadPrograms() {
  try {
    programs.value = (await get('/watches/programs')) as ProgramView[];
  } catch {
    // Supplementary: without the registry a program still shows as its ref.
  }
}

// ── Right pane: tabs ────────────────────────────────────────────────────────
type Tab = 'activity' | 'script' | 'config';
const tab = ref<Tab>('activity');

// ── Activity: the round history + execution log ────────────────────────────
const runs = ref<WatchRun[]>([]);
const runsError = ref('');
// Per-run expansion of the execution log, keyed by run id.
const expanded = reactive<Record<number, boolean>>({});
// The last Run now / Dry-run result, shown inline above the history.
const lastRun = ref<{ outcome: string; summary: string; dry: boolean } | null>(null);

async function loadRuns() {
  if (!selectedId.value) return;
  try {
    runs.value = (await get(`/watches/${selectedId.value}/runs?limit=50`)) as WatchRun[];
    runsError.value = '';
  } catch (e) {
    runsError.value = (e as Error).message;
  }
}

// A round's wall-clock as a compact label (e.g. "42ms", "1.3s", "12s").
function formatMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  return `${s.toFixed(s < 10 ? 1 : 0)}s`;
}

// ── Controls ────────────────────────────────────────────────────────────────
function adopt(w: Watch) {
  const i = watches.value.findIndex((x) => x.id === w.id);
  if (i >= 0) watches.value[i] = w;
}

async function toggleEnabled(w: Watch) {
  busy.value = true;
  error.value = '';
  try {
    adopt((await patch(`/watches/${w.id}`, { enabled: !w.enabled })) as Watch);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function run(dry: boolean) {
  if (!selected.value) return;
  busy.value = true;
  error.value = '';
  try {
    const res = (await post(`/watches/${selected.value.id}/run`, { dry_run: dry })) as WatchRunResult;
    lastRun.value = { outcome: res.outcome, summary: res.summary, dry };
    tab.value = 'activity';
    adopt((await get(`/watches/${selected.value.id}`)) as Watch);
    await loadRuns();
    // Surface the fresh round's log without an extra click.
    if (runs.value.length) expanded[runs.value[0].id] = true;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function remove() {
  const w = selected.value;
  if (!w || isBuiltin(w)) return;
  if (!confirm(`Delete watch "${w.name}"? This can't be undone.`)) return;
  busy.value = true;
  error.value = '';
  try {
    await del(`/watches/${w.id}`);
    watches.value = watches.value.filter((x) => x.id !== w.id);
    selectedId.value = watches.value[0]?.id ?? '';
    router.replace(selectedId.value ? `/watches/${selectedId.value}` : '/watches');
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

// ── Config editing ──────────────────────────────────────────────────────────
const editing = ref(false);
const notice = ref('');
const draft = reactive({
  prompt: '',
  capabilities: {} as Record<string, boolean>,
  model: '',
  effort: '',
  cooldown: 0,
});

function syncDraft(w: Watch) {
  draft.prompt = promptOf(w);
  draft.capabilities = Object.fromEntries(
    GRANTABLE_CAPABILITIES.map((c) => [c, w.capabilities.includes(c)]),
  );
  draft.model = w.model;
  draft.effort = w.effort;
  draft.cooldown = w.cooldown_secs;
}

function startEdit() {
  if (selected.value) syncDraft(selected.value);
  editing.value = true;
}

function cancelEdit() {
  editing.value = false;
}

async function saveConfig() {
  const w = selected.value;
  if (!w) return;
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    const body: Record<string, unknown> = {
      params: draft.prompt.trim() ? { prompt: draft.prompt.trim() } : {},
      capabilities: capabilitiesFrom(draft.capabilities),
      model: draft.model,
      effort: draft.effort,
      cooldown_secs: Number(draft.cooldown) || 0,
    };
    adopt((await patch(`/watches/${w.id}`, body)) as Watch);
    editing.value = false;
    notice.value = 'Saved.';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

// ── Create form ─────────────────────────────────────────────────────────────
// Registers a new watch in the right pane. Builtins are seeded automatically,
// so this is for custom programs — or a second instance of a stock program
// with its own name, prompt, and scope.
const creating = ref(false);
type TriggerKind = 'auto' | 'cron' | 'every' | 'on';
const form = reactive({
  name: '',
  triggerKind: 'auto' as TriggerKind,
  cron: '0 * * * *',
  every: '30m',
  on: 'pr.opened',
  program: 'builtin:status',
  customProgram: '',
  prompt: '',
  scopeAttention: '',
  repo: '',
  capabilities: { mark: true, escalate: true, nudge: false, interrupt: false, launch: false } as Record<string, boolean>,
});

function resetForm() {
  form.name = '';
  form.triggerKind = 'auto';
  form.cron = '0 * * * *';
  form.every = '30m';
  form.on = 'pr.opened';
  form.program = 'builtin:status';
  form.customProgram = '';
  form.prompt = '';
  form.scopeAttention = '';
  form.repo = '';
  form.capabilities = { mark: true, escalate: true, nudge: false, interrupt: false, launch: false };
}

function openCreate() {
  resetForm();
  creatingNew.value = true;
}

// Prefill from a builtin's suggested defaults when the program changes. The
// script declares its own subscriptions, so default to honouring them (auto).
function applyProgramDefaults(programRef: string) {
  const p = programs.value.find((x) => x.program === programRef);
  if (!p) return;
  const t = p.defaults?.trigger ?? {};
  form.triggerKind = 'auto';
  if (t.cron) form.cron = t.cron;
  if (t.every) form.every = t.every;
  if (Array.isArray(t.on) && t.on.length) form.on = t.on.join(', ');
  else if (t.event) form.on = t.level ? `${t.event}=${t.level}` : t.event;
  form.scopeAttention = p.defaults?.scope?.attention ?? '';
  const granted = p.defaults?.capabilities ?? [];
  for (const c of GRANTABLE_CAPABILITIES) form.capabilities[c] = granted.includes(c);
}

async function create() {
  if (!form.name.trim()) return;
  creating.value = true;
  error.value = '';
  try {
    // `auto` omits the trigger entirely so the server reconciles it from the
    // script's manifest; the explicit kinds build the trigger here. (A repo pin
    // rides on `scope` below either way, so `auto` keeps repo scoping.)
    let trigger: Record<string, unknown> | undefined;
    if (form.triggerKind !== 'auto') {
      const t: Record<string, unknown> = {};
      if (form.triggerKind === 'cron' && form.cron.trim()) t.cron = form.cron.trim();
      else if (form.triggerKind === 'every' && form.every.trim()) t.every = form.every.trim();
      else if (form.triggerKind === 'on' && form.on.trim()) {
        t.on = form.on
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean);
      }
      if (form.repo.trim()) t.repo = form.repo.trim();
      // Only override the manifest when the user actually gave a firing
      // condition — a blank explicit kind would create a dead, manual-only
      // watch; fall back to `auto` (server reconciles) instead.
      if (t.cron || t.every || (Array.isArray(t.on) && t.on.length)) trigger = t;
    }

    const scope: Record<string, string> = {};
    if (form.scopeAttention.trim()) scope.attention = form.scopeAttention.trim();
    if (form.repo.trim()) scope.repo = form.repo.trim();

    const programRef = form.program === 'custom' ? form.customProgram.trim() : form.program;
    // Start from the program's suggested params, the prompt layered on top.
    const chosen = programs.value.find((p) => p.program === programRef);
    const params: Record<string, unknown> = { ...(chosen?.defaults?.params ?? {}) };
    if (form.prompt.trim()) params.prompt = form.prompt.trim();

    const body: Record<string, unknown> = {
      name: form.name.trim(),
      scope,
      program: programRef || 'builtin:status',
      params,
      capabilities: capabilitiesFrom(form.capabilities),
      // New watches go live immediately; the per-row toggle disables later.
      enabled: true,
    };
    if (trigger !== undefined) body.trigger = trigger;
    const made = (await post('/watches', body)) as Watch;
    creatingNew.value = false;
    await load();
    selectedId.value = made.id;
    router.push(`/watches/${made.id}`);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

// ── Lifecycle ───────────────────────────────────────────────────────────────
// The route param drives selection (deep links, back/forward). On change,
// reset the pane's transient state and pull the new watch's history.
watchEffect(
  () => props.id,
  (id) => {
    if (id && id !== selectedId.value) {
      selectedId.value = id;
      creatingNew.value = false;
    }
  },
  { immediate: true },
);

watchEffect(selectedId, () => {
  lastRun.value = null;
  runsError.value = '';
  runs.value = [];
  editing.value = false;
  notice.value = '';
  tab.value = 'activity';
  Object.keys(expanded).forEach((k) => delete expanded[Number(k)]);
  loadRuns();
});

onMounted(() => {
  load().then(loadRuns);
  loadPrograms();
});
// Kept alive across navigation (App.vue): refresh on every return, guarded so
// the initial mount doesn't fetch twice.
let firstActivate = true;
onActivated(() => {
  if (firstActivate) {
    firstActivate = false;
    return;
  }
  load();
  loadRuns();
});
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <!-- Toolbar: title, counts, primary action. -->
    <div class="flex min-h-10 flex-wrap items-center gap-2.5 border-b border-line px-5 py-1.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Watches</h1>
      <span v-if="loaded" class="pill" data-testid="watch-count">{{ watches.length }}</span>
      <span v-if="loaded" class="font-mono text-2xs text-faint" data-testid="watch-active-count">
        {{ activeCount }} active
      </span>
      <span class="hidden text-2xs text-faint sm:inline">
        · programs that watch the fleet and mark, nudge, or escalate — also
        <code>loom watch</code>
      </span>
      <button
        type="button"
        data-testid="watch-new"
        class="ml-auto px-2.5 py-1 text-xs font-medium"
        :class="creatingNew ? 'btn-secondary' : 'btn-primary'"
        @click="creatingNew ? (creatingNew = false) : openCreate()"
      >
        {{ creatingNew ? 'Cancel' : 'New watch' }}
      </button>
    </div>

    <p v-if="error" class="border-b border-line px-5 py-2 text-sm text-block">{{ error }}</p>

    <div class="flex min-h-0 flex-1">
      <!-- Master: one row per watch. -->
      <aside class="flex w-64 shrink-0 flex-col overflow-y-auto border-r border-line sm:w-72">
        <p v-if="!loaded" class="px-3 py-3 text-sm text-muted">Loading…</p>
        <div
          v-else-if="!watches.length"
          data-testid="watch-empty"
          class="m-3 rounded border border-dashed border-line bg-surface p-4 text-center"
        >
          <p class="mb-1 text-sm text-muted">No watches yet.</p>
          <p class="text-xs text-faint">
            Builtins are seeded when the daemon starts; add a custom one with
            “New watch”.
          </p>
        </div>
        <ul v-else class="fade-in" data-testid="watch-list">
          <li v-for="w in watches" :key="w.id">
            <button
              type="button"
              data-testid="watch-row"
              :data-watch-id="w.id"
              :data-selected="w.id === selectedId && !creatingNew ? 'true' : 'false'"
              class="relative block w-full border-b border-line px-3 py-2 text-left transition-colors hover:bg-subtle"
              :class="w.id === selectedId && !creatingNew ? 'bg-subtle' : ''"
              @click="select(w)"
            >
              <span
                v-if="w.id === selectedId && !creatingNew"
                class="absolute inset-y-0 left-0 w-0.5 bg-accent"
                aria-hidden="true"
              ></span>
              <span class="flex items-center gap-2">
                <!-- Active dot: green when enabled, hollow when off. -->
                <span
                  class="h-2 w-2 shrink-0 rounded-full"
                  :class="w.enabled ? 'bg-ok-line' : 'border border-faint'"
                  :title="w.enabled ? 'Active' : 'Off'"
                  data-testid="watch-active-dot"
                  :data-active="w.enabled ? 'true' : 'false'"
                ></span>
                <span class="min-w-0 flex-1 truncate text-sm font-semibold text-fg">{{ w.name }}</span>
                <OutcomeBadge :outcome="w.last_outcome" />
              </span>
              <span class="mt-1 flex items-baseline gap-2 pl-4">
                <span class="min-w-0 truncate font-mono text-2xs text-faint">{{ w.program }}</span>
                <span class="ml-auto shrink-0 font-mono text-2xs text-faint">
                  {{ w.last_run_at ? timeAgo(w.last_run_at) : '' }}
                </span>
              </span>
            </button>
          </li>
        </ul>
      </aside>

      <!-- Detail: the selected watch, or the create form. -->
      <section class="min-w-0 flex-1 overflow-y-auto">
        <!-- Create form. -->
        <form
          v-if="creatingNew"
          data-testid="watch-form"
          class="max-w-2xl space-y-3 p-5"
          autocomplete="off"
          @submit.prevent="create"
        >
          <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">New watch</h2>
          <div>
            <label class="mb-1 block text-xs text-muted">Name — unique, used as its handle</label>
            <input
              v-model="form.name"
              data-testid="watch-name"
              placeholder="status-strict"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
            />
          </div>

          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="mb-1 block text-xs text-muted">Program</label>
              <select
                v-model="form.program"
                data-testid="watch-program"
                class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
                @change="applyProgramDefaults(form.program)"
              >
                <option v-for="p in programs" :key="p.program" :value="p.program">
                  {{ p.program }}
                </option>
                <option value="custom">custom path…</option>
              </select>
              <input
                v-if="form.program === 'custom'"
                v-model="form.customProgram"
                data-testid="watch-custom-program"
                placeholder="/home/you/.weaver/watches/my-watch.py"
                autocomplete="off"
                spellcheck="false"
                class="mt-2 w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
              />
            </div>
            <div>
              <label class="mb-1 block text-xs text-muted">Scope — attention filter</label>
              <input
                v-model="form.scopeAttention"
                placeholder="!ok (blank = whole fleet)"
                autocomplete="off"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
              />
            </div>
          </div>

          <div>
            <label class="mb-1 block text-xs text-muted">Trigger — what wakes a round</label>
            <div class="mb-2 inline-flex overflow-hidden rounded border border-line text-xs">
              <button
                v-for="k in (['auto', 'cron', 'every', 'on'] as const)"
                :key="k"
                type="button"
                class="border-l border-line px-3 py-1 first:border-l-0"
                :class="form.triggerKind === k ? 'bg-accent text-accent-fg' : 'bg-input text-muted hover:bg-subtle'"
                @click="form.triggerKind = k"
              >
                {{ k === 'auto' ? 'From script' : k === 'cron' ? 'Cron' : k === 'every' ? 'Every' : 'On events' }}
              </button>
            </div>
            <p v-if="form.triggerKind === 'auto'" class="text-xs text-faint">
              Wakes on the events the script subscribes to (its manifest) — the
              recommended default, so the script decides what it reacts to.
            </p>
            <input
              v-else-if="form.triggerKind === 'cron'"
              v-model="form.cron"
              placeholder="0 * * * *"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
            />
            <input
              v-else-if="form.triggerKind === 'every'"
              v-model="form.every"
              placeholder="30m"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
            />
            <div v-else>
              <input
                v-model="form.on"
                placeholder="pr.merged, session.exited=error"
                autocomplete="off"
                spellcheck="false"
                class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
              />
              <p class="mt-1 text-xs text-faint">
                Comma-separated trigger events:
                <code>session.started/idle/exited/attention/stale</code>,
                <code>triage.changed</code>,
                <code>pr.opened/checks_red/checks_green/merged/review_changed</code>.
                Append <code>=level</code> to filter (e.g. <code>session.attention=blocked</code>).
              </p>
            </div>
          </div>

          <div>
            <label class="mb-1 block text-xs text-muted">
              Prompt — the judgement the stock program runs each round
            </label>
            <textarea
              v-model="form.prompt"
              rows="3"
              placeholder="If a session looks stuck, mark it attention with a one-line reason and nudge a concrete next step."
              autocomplete="off"
              class="w-full resize-y rounded bg-input px-2 py-1.5 text-sm outline-none ring-accent focus:ring-1"
            ></textarea>
          </div>

          <div>
            <label class="mb-1 block text-xs text-muted">
              Repository — optional; pins the watch to one repo (blank = whole fleet)
            </label>
            <input
              v-model="form.repo"
              placeholder="/home/you/code/project"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
            />
          </div>

          <div>
            <label class="mb-1 block text-xs text-muted">
              Capabilities — the intervention ladder (<code>observe</code> always on)
            </label>
            <div class="flex flex-wrap gap-3">
              <label
                v-for="c in GRANTABLE_CAPABILITIES"
                :key="c"
                class="flex items-center gap-1.5 text-sm text-muted"
              >
                <input
                  type="checkbox"
                  v-model="form.capabilities[c]"
                  :data-testid="`cap-${c}`"
                  class="accent-accent"
                />
                <span class="font-mono">{{ c }}</span>
              </label>
            </div>
          </div>

          <button
            type="submit"
            data-testid="watch-create"
            :disabled="creating || !form.name.trim()"
            class="btn-primary px-3 py-1.5 text-sm font-medium"
          >
            {{ creating ? 'Creating…' : 'Create' }}
          </button>
        </form>

        <!-- Empty selection. -->
        <div
          v-else-if="loaded && !selected"
          class="m-5 rounded border border-dashed border-line bg-surface p-6 text-center"
        >
          <p class="text-sm text-muted">Select a watch to see its activity, script, and config.</p>
        </div>

        <!-- Detail. -->
        <template v-else-if="selected">
          <!-- Header: identity + lifecycle controls. -->
          <div class="border-b border-line px-5 py-3" data-testid="watch-detail">
            <div class="flex flex-wrap items-center gap-2.5">
              <h2 class="truncate text-base font-semibold" data-testid="watch-title">
                {{ selected.name }}
              </h2>
              <span
                v-if="isBuiltin(selected)"
                class="meta-chip"
                title="A stock program shipped with loom — disable it rather than delete it"
              >builtin</span>
              <OutcomeBadge :outcome="selected.last_outcome" />
              <div class="ml-auto flex items-center gap-2">
                <label class="mr-1 flex items-center gap-2 text-xs text-muted">
                  <ToggleSwitch
                    :model-value="selected.enabled"
                    :disabled="busy"
                    data-testid="watch-enabled-toggle"
                    @update:model-value="toggleEnabled(selected)"
                  />
                  {{ selected.enabled ? 'Active' : 'Off' }}
                </label>
                <button
                  type="button"
                  data-testid="watch-run"
                  :disabled="busy"
                  class="btn-primary px-2.5 py-1 text-xs font-medium"
                  @click="run(false)"
                >
                  Run now
                </button>
                <button
                  type="button"
                  data-testid="watch-dryrun"
                  :disabled="busy"
                  class="btn-secondary px-2.5 py-1 text-xs font-medium"
                  @click="run(true)"
                >
                  Dry-run
                </button>
              </div>
            </div>
            <div class="mt-2 flex flex-wrap items-center gap-2 text-xs">
              <span class="meta-chip">{{ triggerSummary(selected.trigger) }}</span>
              <span class="meta-chip">{{ scopeSummary(selected.scope) }}</span>
              <span class="font-mono text-faint">{{ selected.program }}</span>
              <span
                v-if="selected.trigger.repo || selected.scope.repo"
                :title="selected.trigger.repo || selected.scope.repo"
                class="meta-chip"
              >📁 {{ repoLabel(selected.trigger.repo || selected.scope.repo || '') }}</span>
              <span class="ml-auto font-mono text-2xs text-faint">
                <span v-if="selected.last_run_at">last run {{ timeAgo(selected.last_run_at) }}</span>
                <span v-else>never run</span>
                <span v-if="selected.enabled && selected.next_run_at"> · next {{ timeAgo(selected.next_run_at) }}</span>
                <span v-if="selected.enabled && selected.wake_at"> · recheck {{ timeAgo(selected.wake_at) }}</span>
              </span>
            </div>
            <p v-if="programInfo" class="mt-2 max-w-3xl text-xs leading-relaxed text-faint">
              {{ programInfo.description }}
            </p>

            <!-- Tabs. -->
            <div class="mt-3 flex gap-1" role="tablist">
              <button
                v-for="t in ([['activity', 'Activity'], ['script', 'Script'], ['config', 'Config']] as const)"
                :key="t[0]"
                type="button"
                role="tab"
                :aria-selected="tab === t[0]"
                :data-testid="`watch-tab-${t[0]}`"
                class="rounded px-2.5 py-1 text-xs font-medium"
                :class="tab === t[0] ? 'bg-subtle text-fg' : 'text-muted hover:bg-subtle/60 hover:text-fg'"
                @click="tab = t[0]"
              >
                {{ t[1] }}
              </button>
            </div>
          </div>

          <!-- Activity: inline run result + the audit trail with logs. -->
          <div v-if="tab === 'activity'" class="p-5">
            <div
              v-if="lastRun"
              data-testid="watch-run-result"
              class="mb-4 flex items-start gap-2 rounded border border-line bg-surface px-3 py-2 text-sm"
            >
              <OutcomeBadge :outcome="lastRun.outcome || null" />
              <span class="text-muted">
                <span v-if="lastRun.dry" class="text-faint">(dry-run) </span>
                {{ lastRun.summary || 'No summary.' }}
              </span>
            </div>

            <p v-if="runsError" class="text-sm text-block">
              Couldn't load round history: {{ runsError }}
            </p>
            <div
              v-else-if="!runs.length"
              class="rounded border border-dashed border-line bg-surface p-6 text-center"
            >
              <p class="text-sm text-muted">No rounds yet.</p>
              <p class="mt-1 text-xs text-faint">Run one now (or dry-run it) to populate the log.</p>
            </div>

            <ul v-else data-testid="watch-runs" class="space-y-2">
              <li
                v-for="r in runs"
                :key="r.id"
                data-testid="watch-run-row"
                class="rounded border border-line bg-surface"
              >
                <button
                  type="button"
                  data-testid="watch-run-toggle"
                  class="w-full p-3 text-left"
                  @click="expanded[r.id] = !expanded[r.id]"
                >
                  <div class="flex flex-wrap items-center gap-2">
                    <OutcomeBadge :outcome="r.outcome" />
                    <span class="text-sm text-muted">{{ r.summary || 'No summary.' }}</span>
                    <span class="ml-auto font-mono text-xs text-faint">{{ timeAgo(r.started_at) }}</span>
                  </div>
                  <div class="mt-1 flex flex-wrap items-center gap-2 text-xs text-faint">
                    <span class="meta-chip">{{ r.trigger_reason || r.trigger_event || 'manual' }}</span>
                    <span v-if="r.duration_ms != null" class="meta-chip">{{ formatMs(r.duration_ms) }}</span>
                    <span v-if="r.exit_code != null" class="meta-chip">exit {{ r.exit_code }}</span>
                    <span v-if="r.actions && r.actions.length">
                      {{ r.actions.length }} action{{ r.actions.length === 1 ? '' : 's' }}
                    </span>
                    <span class="ml-auto text-accent">{{ expanded[r.id] ? 'Hide' : 'Details' }}</span>
                  </div>
                </button>

                <div
                  v-if="expanded[r.id]"
                  data-testid="watch-run-detail"
                  class="space-y-3 border-t border-line p-3"
                >
                  <ul
                    v-if="r.actions && r.actions.length"
                    data-testid="watch-run-actions"
                    class="space-y-1"
                  >
                    <li v-for="(a, j) in r.actions" :key="j" class="flex items-start gap-2 text-xs">
                      <span class="meta-chip shrink-0">
                        <span v-if="a.would" class="text-faint">would </span>{{ a.action || a.would || 'action' }}
                        <span v-if="a.level" class="text-faint">={{ a.level }}</span>
                      </span>
                      <span v-if="a.session" class="shrink-0 font-mono text-faint">{{ a.session }}</span>
                      <span class="min-w-0 text-muted">{{ a.note || a.text || '' }}</span>
                    </li>
                  </ul>

                  <div v-if="r.stdout">
                    <h3 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-faint">stdout</h3>
                    <pre
                      data-testid="watch-run-stdout"
                      class="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-input p-2 font-mono text-xs"
                    >{{ r.stdout }}</pre>
                  </div>
                  <div v-if="r.stderr">
                    <h3 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-faint">stderr</h3>
                    <pre
                      data-testid="watch-run-stderr"
                      class="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-input p-2 font-mono text-xs text-block"
                    >{{ r.stderr }}</pre>
                  </div>

                  <p
                    v-if="!(r.actions && r.actions.length) && !r.stdout && !r.stderr"
                    class="text-xs text-faint"
                  >
                    No actions or output recorded.
                  </p>
                </div>
              </li>
            </ul>
          </div>

          <!-- Script: the program source, read-only for builtins. -->
          <div v-else-if="tab === 'script'" class="p-5">
            <template v-if="programInfo">
              <p class="mb-2 text-xs text-faint">
                <span class="font-mono">{{ programInfo.program }}</span> —
                {{ programInfo.title }}. A stock script shipped inside loom; the
                source is read-only. Start a custom watch from a copy with
                <code>loom watch new &lt;name&gt;</code>.
              </p>
              <pre
                data-testid="watch-script"
                class="overflow-auto rounded border border-line bg-input p-3 font-mono text-xs leading-relaxed"
              >{{ programInfo.source }}</pre>
            </template>
            <div
              v-else
              class="rounded border border-dashed border-line bg-surface p-6 text-center"
            >
              <p class="text-sm text-muted">
                Custom program: <span class="font-mono">{{ selected.program }}</span>
              </p>
              <p class="mt-1 text-xs text-faint">
                The script lives on the server's disk; edit it there — each round
                runs the file as it is on disk.
              </p>
            </div>
          </div>

          <!-- Config: the tuneable knobs + warm session. -->
          <div v-else class="max-w-3xl p-5">
            <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>
            <section class="rounded border border-line bg-surface p-4">
              <div class="mb-3 flex items-center justify-between">
                <h3 class="text-2xs font-semibold uppercase tracking-wider text-muted">Config</h3>
                <button
                  v-if="!editing"
                  type="button"
                  data-testid="watch-edit"
                  class="btn-secondary px-2.5 py-1 text-xs font-medium"
                  @click="startEdit"
                >
                  Edit
                </button>
                <div v-else class="flex gap-2">
                  <button
                    type="button"
                    data-testid="watch-save"
                    :disabled="busy"
                    class="btn-primary px-2.5 py-1 text-xs font-medium"
                    @click="saveConfig"
                  >
                    Save
                  </button>
                  <button
                    type="button"
                    :disabled="busy"
                    class="btn-secondary px-2.5 py-1 text-xs font-medium"
                    @click="cancelEdit"
                  >
                    Cancel
                  </button>
                </div>
              </div>

              <dl class="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-2 text-sm">
                <dt class="text-faint">Trigger</dt>
                <dd class="font-mono">{{ triggerSummary(selected.trigger) }}</dd>
                <dt class="text-faint">Scope</dt>
                <dd class="font-mono">{{ scopeSummary(selected.scope) }}</dd>
                <dt class="text-faint">Program</dt>
                <dd class="font-mono">{{ selected.program }}</dd>

                <dt class="pt-1 text-faint">Capabilities</dt>
                <dd>
                  <div v-if="!editing" class="flex flex-wrap gap-1.5">
                    <span v-for="c in selected.capabilities" :key="c" class="meta-chip">{{ c }}</span>
                  </div>
                  <div v-else class="flex flex-wrap gap-3">
                    <label
                      v-for="c in GRANTABLE_CAPABILITIES"
                      :key="c"
                      class="flex items-center gap-1.5 text-sm text-muted"
                    >
                      <input
                        type="checkbox"
                        v-model="draft.capabilities[c]"
                        :data-testid="`cap-${c}`"
                        class="accent-accent"
                      />
                      <span class="font-mono">{{ c }}</span>
                    </label>
                  </div>
                </dd>

                <dt class="text-faint">Model / effort</dt>
                <dd v-if="!editing" class="font-mono">
                  {{ selected.model || 'default' }} / {{ selected.effort || 'default' }}
                </dd>
                <dd v-else class="flex gap-2">
                  <input
                    v-model="draft.model"
                    placeholder="default"
                    class="w-28 rounded bg-input px-2 py-1 font-mono text-sm outline-none ring-accent focus:ring-1"
                  />
                  <input
                    v-model="draft.effort"
                    placeholder="default"
                    class="w-28 rounded bg-input px-2 py-1 font-mono text-sm outline-none ring-accent focus:ring-1"
                  />
                </dd>

                <dt class="text-faint">Cooldown</dt>
                <dd v-if="!editing" class="font-mono">{{ selected.cooldown_secs }}s</dd>
                <dd v-else>
                  <input
                    v-model.number="draft.cooldown"
                    type="number"
                    min="0"
                    class="w-28 rounded bg-input px-2 py-1 font-mono text-sm outline-none ring-accent focus:ring-1"
                  />
                  <span class="ml-1 text-xs text-faint">seconds</span>
                </dd>

                <dt class="pt-1 text-faint">Prompt</dt>
                <dd>
                  <p
                    v-if="!editing"
                    data-testid="watch-prompt"
                    class="whitespace-pre-wrap text-sm text-fg"
                  >
                    {{ promptOf(selected) || '— (no prompt; the program decides)' }}
                  </p>
                  <textarea
                    v-else
                    v-model="draft.prompt"
                    data-testid="watch-prompt-input"
                    rows="4"
                    placeholder="If a session looks stuck, mark it attention and nudge a concrete next step."
                    class="w-full resize-y rounded bg-input px-2 py-1.5 text-sm outline-none ring-accent focus:ring-1"
                  ></textarea>
                </dd>
              </dl>
            </section>

            <!-- Warm session: its live terminal when warm mode is on. -->
            <section v-if="selected.warm_session_id" class="mt-5" data-testid="watch-warm-terminal">
              <h3 class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">Warm session</h3>
              <AgentTerminal :id="selected.warm_session_id" />
            </section>
            <section v-else class="mt-5 rounded border border-dashed border-line bg-surface p-4">
              <h3 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-muted">Warm session</h3>
              <p v-if="selected.warm" class="text-xs text-faint" data-testid="watch-warm-pending">
                Warm mode is on. The engine brings up a persistent session on the
                next round; its live terminal appears here, and it carries memory
                from one round to the next.
              </p>
              <p v-else class="text-xs text-faint" data-testid="watch-warm-off">
                Each round runs fresh. Turn on warm mode (set <code>params.warm</code>)
                to keep one persistent session with across-round memory — its
                terminal lives here.
              </p>
            </section>

            <!-- Deletion: custom watches only; builtins re-seed on boot. -->
            <section v-if="!isBuiltin(selected)" class="mt-5">
              <button
                type="button"
                data-testid="watch-delete"
                :disabled="busy"
                class="btn-danger px-2.5 py-1 text-xs font-medium"
                @click="remove"
              >
                Delete watch
              </button>
            </section>
          </div>
        </template>
      </section>
    </div>
  </div>
</template>
