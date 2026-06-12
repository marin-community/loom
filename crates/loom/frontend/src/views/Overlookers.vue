<script setup lang="ts">
import { ref, reactive, onMounted } from 'vue';
import { get, post, patch } from '../api';
import type { Overlooker, OverlookerRunResult, ProgramView } from '../types';
import OutcomeBadge from '../components/OutcomeBadge.vue';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import { timeAgo } from '../lib/time';
import {
  triggerSummary,
  scopeSummary,
  repoLabel,
  capabilitiesFrom,
  GRANTABLE_CAPABILITIES,
} from '../lib/overlooker';

// The Overlooker panel — infrastructure that watches the fleet, sibling to the
// session list. API-first: every row is an `OverlookerView`, every control a
// REST call. See docs/plans/overlooker.md "The panel (loom UI)".
const overlookers = ref<Overlooker[]>([]);
const loaded = ref(false);
const error = ref('');
// Per-id transient "Run now / Dry-run" result line, surfaced inline on the row.
const runResults = reactive<Record<string, { outcome: string; summary: string; dry: boolean }>>({});
// Per-id busy flag so a row's buttons disable while its call is in flight.
const busy = reactive<Record<string, boolean>>({});

async function load() {
  try {
    overlookers.value = (await get('/overlookers')) as Overlooker[];
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loaded.value = true;
  }
}

// The builtin program registry — what the create form offers and the "Builtin
// programs" section lists (script sources shown read-only).
const programs = ref<ProgramView[]>([]);
// The program whose script source is expanded in the registry section.
const expandedSource = ref('');

async function loadPrograms() {
  try {
    programs.value = (await get('/overlookers/programs')) as ProgramView[];
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function toggleEnabled(o: Overlooker) {
  busy[o.id] = true;
  error.value = '';
  try {
    const updated = (await patch(`/overlookers/${o.id}`, { enabled: !o.enabled })) as Overlooker;
    const i = overlookers.value.findIndex((x) => x.id === o.id);
    if (i >= 0) overlookers.value[i] = updated;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy[o.id] = false;
  }
}

async function run(o: Overlooker, dry: boolean) {
  busy[o.id] = true;
  error.value = '';
  try {
    const res = (await post(`/overlookers/${o.id}/run`, { dry_run: dry })) as OverlookerRunResult;
    runResults[o.id] = { outcome: res.outcome, summary: res.summary, dry };
    // A real run updates last_run_at / last_outcome — refresh just this row.
    const fresh = (await get(`/overlookers/${o.id}`)) as Overlooker;
    const i = overlookers.value.findIndex((x) => x.id === o.id);
    if (i >= 0) overlookers.value[i] = fresh;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy[o.id] = false;
  }
}

// ── Create form ────────────────────────────────────────────────────────────
// A small inline form, modelled on SessionList's. The minimum to register a
// useful overlooker: a name, a trigger (cron / every / event+level), a program
// (default builtin:status), a judgement prompt, the explicit capabilities, and
// an optional repo pin. Everything else takes the server's defaults.
const showForm = ref(false);
const creating = ref(false);
type TriggerKind = 'cron' | 'every' | 'event';
const form = reactive({
  name: '',
  triggerKind: 'cron' as TriggerKind,
  cron: '0 * * * *',
  every: '30m',
  event: 'attention',
  level: 'blocked',
  // A builtin reference from the registry, or the literal 'custom' to free-type
  // a program file path into `customProgram`.
  program: 'builtin:status',
  customProgram: '',
  prompt: '',
  scopeAttention: '!ok',
  repo: '',
  capabilities: { mark: true, escalate: true, nudge: false, interrupt: false, launch: false } as Record<string, boolean>,
});

function resetForm() {
  form.name = '';
  form.triggerKind = 'cron';
  form.cron = '0 * * * *';
  form.every = '30m';
  form.event = 'attention';
  form.level = 'blocked';
  form.program = 'builtin:status';
  form.customProgram = '';
  form.prompt = '';
  form.scopeAttention = '!ok';
  form.repo = '';
  form.capabilities = { mark: true, escalate: true, nudge: false, interrupt: false, launch: false };
}

// Prefill the form from a builtin's suggested defaults (trigger cadence, scope,
// capability grants) — the registry's starting point, freely editable after.
function applyProgramDefaults(programRef: string) {
  const p = programs.value.find((x) => x.program === programRef);
  if (!p) return;
  const t = p.defaults?.trigger ?? {};
  if (t.cron) {
    form.triggerKind = 'cron';
    form.cron = t.cron;
  } else if (t.every) {
    form.triggerKind = 'every';
    form.every = t.every;
  } else if (t.event) {
    form.triggerKind = 'event';
    form.event = t.event;
    form.level = t.level ?? '';
  }
  form.scopeAttention = p.defaults?.scope?.attention ?? '';
  const granted = p.defaults?.capabilities ?? [];
  for (const c of GRANTABLE_CAPABILITIES) form.capabilities[c] = granted.includes(c);
}

// "Use" on a registry row: open the create form prefilled with that program.
function useProgram(p: ProgramView) {
  form.program = p.program;
  applyProgramDefaults(p.program);
  if (!form.name.trim()) form.name = p.program.replace(/^builtin:/, '');
  showForm.value = true;
}

async function create() {
  if (!form.name.trim()) return;
  creating.value = true;
  error.value = '';
  try {
    const trigger: Record<string, string> = {};
    if (form.triggerKind === 'cron' && form.cron.trim()) trigger.cron = form.cron.trim();
    else if (form.triggerKind === 'every' && form.every.trim()) trigger.every = form.every.trim();
    else if (form.triggerKind === 'event' && form.event.trim()) {
      trigger.event = form.event.trim();
      if (form.level.trim()) trigger.level = form.level.trim();
    }
    if (form.repo.trim()) trigger.repo = form.repo.trim();

    const scope: Record<string, string> = {};
    if (form.scopeAttention.trim()) scope.attention = form.scopeAttention.trim();
    if (form.repo.trim()) scope.repo = form.repo.trim();

    // `observe` is implicit; ship the explicitly-ticked grants on top of it.
    const capabilities = capabilitiesFrom(form.capabilities);

    const programRef =
      form.program === 'custom' ? form.customProgram.trim() : form.program;
    // Start from the program's suggested params (e.g. pr-label's label), with
    // the prompt — the form's one explicit param — layered on top.
    const chosen = programs.value.find((p) => p.program === programRef);
    const params: Record<string, unknown> = { ...(chosen?.defaults?.params ?? {}) };
    if (form.prompt.trim()) params.prompt = form.prompt.trim();

    const body: Record<string, unknown> = {
      name: form.name.trim(),
      trigger,
      scope,
      program: programRef || 'builtin:status',
      params,
      capabilities,
    };
    await post('/overlookers', body);
    resetForm();
    showForm.value = false;
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

onMounted(() => {
  load();
  loadPrograms();
});
</script>

<template>
  <div class="px-5 py-3">
    <div class="mb-1 flex min-h-7 flex-wrap items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Overlookers</h1>
      <button
        type="button"
        data-testid="overlooker-new"
        :class="[
          'ml-auto px-2.5 py-1 text-xs font-medium',
          showForm ? 'btn-secondary' : 'btn-primary',
        ]"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New overlooker' }}
      </button>
    </div>
    <p class="text-xs text-faint mb-3">
      Periodic, triggered watch agents over the fleet. Each wakes on a trigger,
      surveys the sessions in scope, and marks / nudges / escalates — a bounded,
      audited intervention ladder. Also driveable from
      <code>loom overlooker</code>.
    </p>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <!-- Create form: the minimum to register a useful overlooker. -->
    <form
      v-if="showForm"
      data-testid="overlooker-form"
      class="mb-5 rounded border border-line bg-surface p-4 space-y-3"
      autocomplete="off"
      @submit.prevent="create"
    >
      <div>
        <label class="block text-xs text-muted mb-1">Name — unique, used as its handle</label>
        <input
          v-model="form.name"
          data-testid="overlooker-name"
          placeholder="status-check"
          autocomplete="off"
          spellcheck="false"
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
        />
      </div>

      <div>
        <label class="block text-xs text-muted mb-1">Trigger — what wakes a round</label>
        <div class="inline-flex rounded border border-line text-xs overflow-hidden mb-2">
          <button
            v-for="k in (['cron', 'every', 'event'] as const)"
            :key="k"
            type="button"
            :class="[
              'px-3 py-1 border-l border-line first:border-l-0',
              form.triggerKind === k ? 'bg-accent text-accent-fg' : 'bg-input text-muted hover:bg-subtle',
            ]"
            @click="form.triggerKind = k"
          >
            {{ k === 'cron' ? 'Cron' : k === 'every' ? 'Every' : 'On event' }}
          </button>
        </div>
        <input
          v-if="form.triggerKind === 'cron'"
          v-model="form.cron"
          placeholder="0 * * * *"
          autocomplete="off"
          spellcheck="false"
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
        />
        <input
          v-else-if="form.triggerKind === 'every'"
          v-model="form.every"
          placeholder="30m"
          autocomplete="off"
          spellcheck="false"
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
        />
        <div v-else class="grid grid-cols-2 gap-3">
          <div>
            <label class="block text-xs text-faint mb-1">Event kind</label>
            <input
              v-model="form.event"
              placeholder="attention"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
          </div>
          <div>
            <label class="block text-xs text-faint mb-1">Level (optional)</label>
            <input
              v-model="form.level"
              placeholder="blocked"
              autocomplete="off"
              spellcheck="false"
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
          </div>
        </div>
      </div>

      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="block text-xs text-muted mb-1">Program</label>
          <select
            v-model="form.program"
            data-testid="overlooker-program"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
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
            data-testid="overlooker-custom-program"
            placeholder="/home/you/.weaver/overlookers/my-watch.py"
            autocomplete="off"
            spellcheck="false"
            class="mt-2 w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
          />
        </div>
        <div>
          <label class="block text-xs text-muted mb-1">Scope — attention filter</label>
          <input
            v-model="form.scopeAttention"
            placeholder="!ok"
            autocomplete="off"
            spellcheck="false"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
          />
        </div>
      </div>

      <div>
        <label class="block text-xs text-muted mb-1">
          Prompt — the judgement the stock program runs each round
        </label>
        <textarea
          v-model="form.prompt"
          rows="3"
          placeholder="If a session looks stuck, mark it attention with a one-line reason and nudge a concrete next step."
          autocomplete="off"
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
        ></textarea>
      </div>

      <div>
        <label class="block text-xs text-muted mb-1">
          Repository — optional; pins the overlooker to one repo (blank = whole fleet)
        </label>
        <input
          v-model="form.repo"
          placeholder="/home/you/code/project"
          autocomplete="off"
          spellcheck="false"
          class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
        />
      </div>

      <div>
        <label class="block text-xs text-muted mb-1">
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
        data-testid="overlooker-create"
        :disabled="creating || !form.name.trim()"
        class="btn-primary px-3 py-1.5 text-sm font-medium"
      >
        {{ creating ? 'Creating…' : 'Create' }}
      </button>
    </form>

    <!-- Empty state. -->
    <div
      v-if="loaded && !overlookers.length && !showForm"
      data-testid="overlooker-empty"
      class="rounded border border-dashed border-line bg-surface p-6 text-center"
    >
      <p class="text-sm text-muted mb-1">No overlookers yet.</p>
      <p class="text-xs text-faint">
        Create one above, or scaffold a custom program with
        <code>loom overlooker add</code> /
        <code>loom overlooker new &lt;name&gt;</code>.
      </p>
    </div>

    <p v-else-if="!loaded" class="text-muted text-sm">Loading…</p>

    <!-- The list. One row per overlooker. -->
    <ul
      v-if="overlookers.length"
      data-testid="overlooker-list"
      class="overflow-hidden rounded-md border border-line bg-surface"
    >
      <li
        v-for="(o, i) in overlookers"
        :key="o.id"
        data-testid="overlooker-row"
        :data-overlooker-id="o.id"
        :style="{ '--i': i }"
        class="stagger-in border-b border-line px-3 py-2.5 last:border-0"
      >
        <div class="flex items-start gap-3">
          <!-- Identity + at-a-glance state. -->
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 flex-wrap">
              <router-link
                :to="`/overlookers/${o.id}`"
                class="truncate text-sm font-semibold text-fg hover:text-accent"
                data-testid="overlooker-name-link"
              >
                {{ o.name }}
              </router-link>
              <OutcomeBadge :outcome="o.last_outcome" />
              <span v-if="!o.enabled" class="meta-chip">disabled</span>
            </div>
            <div class="mt-1 flex items-center gap-2 flex-wrap text-xs">
              <span class="meta-chip">{{ triggerSummary(o.trigger) }}</span>
              <span class="meta-chip">{{ scopeSummary(o.scope) }}</span>
              <span class="font-mono text-faint">{{ o.program }}</span>
              <span
                v-if="o.trigger.repo"
                :title="o.trigger.repo"
                class="meta-chip"
              >📁 {{ repoLabel(o.trigger.repo) }}</span>
            </div>
            <p class="mt-1 text-xs text-faint">
              <span v-if="o.last_run_at">last run {{ timeAgo(o.last_run_at) }}</span>
              <span v-else>never run</span>
              <span v-if="o.enabled && o.next_run_at"> · next {{ timeAgo(o.next_run_at) }}</span>
            </p>
          </div>

          <!-- Controls. -->
          <div class="flex shrink-0 items-center gap-2">
            <!-- Enabled toggle. data-testid rides on the switch button; state
                 is exposed via the switch role's own aria-checked. -->
            <ToggleSwitch
              :model-value="o.enabled"
              :disabled="busy[o.id]"
              :title="o.enabled ? 'Enabled — click to disable' : 'Disabled — click to enable'"
              data-testid="overlooker-enabled-toggle"
              @update:model-value="toggleEnabled(o)"
            />
            <button
              type="button"
              data-testid="overlooker-run"
              :disabled="busy[o.id]"
              class="btn-primary px-2.5 py-1 text-xs font-medium"
              @click="run(o, false)"
            >
              Run now
            </button>
            <button
              type="button"
              data-testid="overlooker-dryrun"
              :disabled="busy[o.id]"
              class="btn-secondary px-2.5 py-1 text-xs font-medium"
              @click="run(o, true)"
            >
              Dry-run
            </button>
          </div>
        </div>

        <!-- Inline run result — the outcome + summary the round returned. -->
        <div
          v-if="runResults[o.id]"
          data-testid="overlooker-run-result"
          class="mt-2 flex items-start gap-2 rounded bg-subtle/60 px-2.5 py-1.5 text-xs"
        >
          <OutcomeBadge :outcome="runResults[o.id].outcome || null" />
          <span class="text-muted">
            <span v-if="runResults[o.id].dry" class="text-faint">(dry-run) </span>
            {{ runResults[o.id].summary || 'No summary.' }}
          </span>
        </div>
      </li>
    </ul>

    <!-- Builtin programs — the stock programs that ship with loom. Script
         sources are read-only (they live in the weaver repo); "Use" opens the
         create form prefilled with the program's suggested defaults. -->
    <section v-if="programs.length" class="mt-6" data-testid="builtin-programs">
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1">
        Builtin programs
      </h2>
      <p class="text-xs text-faint mb-2">
        Stock programs shipped with loom — pick one as a new overlooker's
        program. Sources are read-only; start a custom one from a copy with
        <code>loom overlooker new &lt;name&gt;</code>.
      </p>
      <ul class="overflow-hidden rounded-md border border-line bg-surface">
        <li
          v-for="p in programs"
          :key="p.program"
          data-testid="program-row"
          class="border-b border-line px-3 py-2.5 last:border-0"
        >
          <div class="flex items-center gap-2 flex-wrap">
            <span class="font-mono text-sm font-semibold">{{ p.program }}</span>
            <span class="text-sm text-muted">{{ p.title }}</span>
            <div class="ml-auto flex shrink-0 items-center gap-2">
              <button
                type="button"
                data-testid="program-source-toggle"
                class="btn-secondary px-2.5 py-1 text-xs font-medium"
                @click="expandedSource = expandedSource === p.program ? '' : p.program"
              >
                {{ expandedSource === p.program ? 'Hide source' : 'View source' }}
              </button>
              <button
                type="button"
                data-testid="program-use"
                class="btn-primary px-2.5 py-1 text-xs font-medium"
                @click="useProgram(p)"
              >
                Use
              </button>
            </div>
          </div>
          <p class="mt-1 text-xs text-faint">{{ p.description }}</p>
          <pre
            v-if="expandedSource === p.program"
            data-testid="program-source"
            class="mt-2 max-h-80 overflow-auto rounded bg-input p-3 text-xs font-mono"
          >{{ p.source }}</pre>
        </li>
      </ul>
    </section>
  </div>
</template>
