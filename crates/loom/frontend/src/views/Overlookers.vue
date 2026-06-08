<script setup lang="ts">
import { ref, reactive, onMounted } from 'vue';
import { get, post, patch } from '../api';
import type { Overlooker, OverlookerRunResult } from '../types';
import OutcomeBadge from '../components/OutcomeBadge.vue';
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
  program: 'builtin:status',
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
  form.prompt = '';
  form.scopeAttention = '!ok';
  form.repo = '';
  form.capabilities = { mark: true, escalate: true, nudge: false, interrupt: false, launch: false };
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

    const body: Record<string, unknown> = {
      name: form.name.trim(),
      trigger,
      scope,
      program: form.program.trim() || 'builtin:status',
      params: form.prompt.trim() ? { prompt: form.prompt.trim() } : {},
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

onMounted(load);
</script>

<template>
  <div>
    <div class="flex items-center gap-3 mb-1">
      <router-link to="/" class="text-muted hover:text-fg text-sm">← all</router-link>
      <h1 class="text-xl font-semibold">Overlookers</h1>
      <button
        type="button"
        data-testid="overlooker-new"
        class="ml-auto rounded bg-accent hover:bg-accent-hover px-3 py-1.5 text-sm font-medium text-accent-fg"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New overlooker' }}
      </button>
    </div>
    <p class="text-xs text-faint mb-4">
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
          <input
            v-model="form.program"
            placeholder="builtin:status"
            autocomplete="off"
            spellcheck="false"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent font-mono"
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
        class="rounded bg-accent hover:bg-accent-hover px-3 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-50"
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
      class="overflow-hidden rounded border border-line bg-surface"
    >
      <li
        v-for="(o, i) in overlookers"
        :key="o.id"
        data-testid="overlooker-row"
        :data-overlooker-id="o.id"
        :style="{ '--i': i }"
        class="stagger-in border-b border-line px-4 py-3 last:border-0"
      >
        <div class="flex items-start gap-3">
          <!-- Identity + at-a-glance state. -->
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 flex-wrap">
              <router-link
                :to="`/overlookers/${o.id}`"
                class="truncate text-base font-semibold text-fg hover:text-accent"
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
            <!-- Enabled toggle. -->
            <button
              type="button"
              data-testid="overlooker-enabled-toggle"
              :aria-pressed="o.enabled"
              :disabled="busy[o.id]"
              :class="[
                'relative inline-flex h-5 w-9 items-center rounded-full transition-colors disabled:opacity-50',
                o.enabled ? 'bg-accent' : 'bg-subtle',
              ]"
              :title="o.enabled ? 'Enabled — click to disable' : 'Disabled — click to enable'"
              @click="toggleEnabled(o)"
            >
              <span
                :class="[
                  'inline-block h-4 w-4 transform rounded-full bg-surface transition-transform',
                  o.enabled ? 'translate-x-4' : 'translate-x-0.5',
                ]"
              ></span>
            </button>
            <button
              type="button"
              data-testid="overlooker-run"
              :disabled="busy[o.id]"
              class="rounded bg-accent hover:bg-accent-hover px-2.5 py-1 text-xs font-medium text-accent-fg disabled:opacity-50"
              @click="run(o, false)"
            >
              Run now
            </button>
            <button
              type="button"
              data-testid="overlooker-dryrun"
              :disabled="busy[o.id]"
              class="rounded bg-subtle hover:bg-subtle-hover px-2.5 py-1 text-xs font-medium disabled:opacity-50"
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
  </div>
</template>
