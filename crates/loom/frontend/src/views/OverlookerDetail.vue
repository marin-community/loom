<script setup lang="ts">
import { ref, reactive, computed, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import { get, post, patch, del } from '../api';
import type { Overlooker, OverlookerRun, OverlookerRunResult, ProgramView } from '../types';
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
} from '../lib/overlooker';

// One overlooker's detail: its config (readable + editable), the round-history
// audit trail (the marks/nudges/would-dos each round took), the lifecycle
// controls, and — for a warm overlooker — the live terminal of its persistent
// session (hidden from the fleet, so this is its home).
const props = defineProps<{ id: string }>();
const router = useRouter();

const ov = ref<Overlooker | null>(null);
const runs = ref<OverlookerRun[]>([]);
const error = ref('');
const notice = ref('');
// A non-fatal note when the round-history fetch fails, so an empty list isn't
// silently confused with a real "no rounds yet".
const runsError = ref('');
const loaded = ref(false);
const busy = ref(false);
// Per-run expansion of the actions list (the audit detail), keyed by run id.
const expanded = reactive<Record<number, boolean>>({});
// The last Run now / Dry-run result, shown inline.
const lastRun = ref<{ outcome: string; summary: string; dry: boolean } | null>(null);

// Editable config draft. Populated from the loaded overlooker; PATCHed on save.
const editing = ref(false);
const draft = reactive({
  prompt: '',
  capabilities: {} as Record<string, boolean>,
  model: '',
  effort: '',
  cooldown: 0,
});

function syncDraft(o: Overlooker) {
  draft.prompt = promptOf(o);
  draft.capabilities = Object.fromEntries(
    GRANTABLE_CAPABILITIES.map((c) => [c, o.capabilities.includes(c)]),
  );
  draft.model = o.model;
  draft.effort = o.effort;
  draft.cooldown = o.cooldown_secs;
}

async function loadOverlooker() {
  try {
    const o = (await get(`/overlookers/${props.id}`)) as Overlooker;
    ov.value = o;
    if (!editing.value) syncDraft(o);
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loaded.value = true;
  }
}

async function loadRuns() {
  try {
    runs.value = (await get(`/overlookers/${props.id}/runs?limit=50`)) as OverlookerRun[];
    runsError.value = '';
  } catch (e) {
    // History is supplementary; a failure here shouldn't blank the page, but
    // surface it so an empty list isn't mistaken for "no rounds yet".
    runsError.value = (e as Error).message;
  }
}

// A round's wall-clock as a compact label (e.g. "42ms", "1.3s", "12s").
function formatMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  return `${s.toFixed(s < 10 ? 1 : 0)}s`;
}

async function adopt(o: Overlooker) {
  ov.value = o;
}

async function toggleEnabled() {
  if (!ov.value) return;
  busy.value = true;
  error.value = '';
  try {
    adopt((await patch(`/overlookers/${props.id}`, { enabled: !ov.value.enabled })) as Overlooker);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function run(dry: boolean) {
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    const res = (await post(`/overlookers/${props.id}/run`, { dry_run: dry })) as OverlookerRunResult;
    lastRun.value = { outcome: res.outcome, summary: res.summary, dry };
    await Promise.all([loadOverlooker(), loadRuns()]);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function saveConfig() {
  if (!ov.value) return;
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    const capabilities = capabilitiesFrom(draft.capabilities);
    const body: Record<string, unknown> = {
      params: draft.prompt.trim() ? { prompt: draft.prompt.trim() } : {},
      capabilities,
      model: draft.model,
      effort: draft.effort,
      cooldown_secs: Number(draft.cooldown) || 0,
    };
    adopt((await patch(`/overlookers/${props.id}`, body)) as Overlooker);
    editing.value = false;
    notice.value = 'Saved.';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

function startEdit() {
  if (ov.value) syncDraft(ov.value);
  editing.value = true;
}

function cancelEdit() {
  if (ov.value) syncDraft(ov.value);
  editing.value = false;
}

async function remove() {
  if (!ov.value) return;
  if (!confirm(`Delete overlooker "${ov.value.name}"? This can't be undone.`)) return;
  busy.value = true;
  error.value = '';
  try {
    await del(`/overlookers/${props.id}`);
    router.push('/overlookers');
  } catch (e) {
    error.value = (e as Error).message;
    busy.value = false;
  }
}

// The capability set as a readable, ordered chip list (observe is implicit).
const grantedCaps = computed(() => ov.value?.capabilities ?? []);

// The builtin program registry, to label this overlooker's program and render
// a builtin script's source read-only (it ships inside the loom binary).
const programs = ref<ProgramView[]>([]);
const showSource = ref(false);
const programInfo = computed(
  () => programs.value.find((p) => p.program === ov.value?.program) ?? null,
);

async function loadPrograms() {
  try {
    programs.value = (await get('/overlookers/programs')) as ProgramView[];
  } catch {
    // Supplementary: without the registry the program still shows as its ref.
  }
}

onMounted(() => {
  loadOverlooker();
  loadRuns();
  loadPrograms();
});
</script>

<template>
  <div class="px-5 py-3">
    <div class="flex items-center gap-3 mb-1">
      <router-link to="/overlookers" class="text-xs text-muted hover:text-fg">← overlookers</router-link>
    </div>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>

    <p v-if="!loaded" class="text-muted text-sm">Loading…</p>

    <template v-if="ov">
      <!-- Header: name, outcome, lifecycle controls. -->
      <div class="flex items-start gap-3 mb-4 flex-wrap">
        <div class="min-w-0">
          <div class="flex items-center gap-2 flex-wrap">
            <h1 class="text-base font-semibold truncate" data-testid="overlooker-title">{{ ov.name }}</h1>
            <OutcomeBadge :outcome="ov.last_outcome" />
          </div>
          <p class="mt-1 text-xs text-faint">
            <span v-if="ov.last_run_at">last run {{ timeAgo(ov.last_run_at) }}</span>
            <span v-else>never run</span>
            <span v-if="ov.enabled && ov.next_run_at"> · next {{ timeAgo(ov.next_run_at) }}</span>
          </p>
        </div>
        <div class="ml-auto flex items-center gap-2">
          <label class="flex items-center gap-2 text-xs text-muted mr-1">
            <ToggleSwitch
              :model-value="ov.enabled"
              :disabled="busy"
              data-testid="overlooker-enabled-toggle"
              @update:model-value="toggleEnabled"
            />
            {{ ov.enabled ? 'Enabled' : 'Disabled' }}
          </label>
          <button
            type="button"
            data-testid="overlooker-run"
            :disabled="busy"
            class="btn-primary px-2.5 py-1 text-xs font-medium"
            @click="run(false)"
          >
            Run now
          </button>
          <button
            type="button"
            data-testid="overlooker-dryrun"
            :disabled="busy"
            class="btn-secondary px-2.5 py-1 text-xs font-medium"
            @click="run(true)"
          >
            Dry-run
          </button>
          <button
            type="button"
            data-testid="overlooker-delete"
            :disabled="busy"
            class="btn-danger px-2.5 py-1 text-xs font-medium"
            @click="remove"
          >
            Delete
          </button>
        </div>
      </div>

      <!-- Inline run result. -->
      <div
        v-if="lastRun"
        data-testid="overlooker-run-result"
        class="mb-4 flex items-start gap-2 rounded border border-line bg-surface px-3 py-2 text-sm"
      >
        <OutcomeBadge :outcome="lastRun.outcome || null" />
        <span class="text-muted">
          <span v-if="lastRun.dry" class="text-faint">(dry-run) </span>
          {{ lastRun.summary || 'No summary.' }}
        </span>
      </div>

      <!-- Config. -->
      <section class="mb-6 rounded border border-line bg-surface p-4">
        <div class="flex items-center justify-between mb-3">
          <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted">Config</h2>
          <button
            v-if="!editing"
            type="button"
            data-testid="overlooker-edit"
            class="btn-secondary px-2.5 py-1 text-xs font-medium"
            @click="startEdit"
          >
            Edit
          </button>
          <div v-else class="flex gap-2">
            <button
              type="button"
              data-testid="overlooker-save"
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

        <!-- Read-only structural facts (trigger / scope / program are not edited
             here — they change shape; the panel edits the tuneable knobs). -->
        <dl class="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-2 text-sm">
          <dt class="text-faint">Trigger</dt>
          <dd class="font-mono">{{ triggerSummary(ov.trigger) }}</dd>
          <dt class="text-faint">Scope</dt>
          <dd class="font-mono">{{ scopeSummary(ov.scope) }}</dd>
          <dt v-if="ov.trigger.repo" class="text-faint">Repository</dt>
          <dd v-if="ov.trigger.repo" class="font-mono" :title="ov.trigger.repo">
            📁 {{ repoLabel(ov.trigger.repo) }}
          </dd>
          <dt class="text-faint">Program</dt>
          <dd>
            <span class="font-mono">{{ ov.program }}</span>
            <button
              v-if="programInfo?.source"
              type="button"
              data-testid="program-source-toggle"
              class="ml-2 text-xs text-accent hover:underline"
              @click="showSource = !showSource"
            >
              {{ showSource ? 'hide source' : 'view source' }}
            </button>
            <p v-if="programInfo" class="mt-0.5 text-xs text-faint">
              {{ programInfo.title }} — a builtin script shipped with loom;
              the source is read-only.
            </p>
            <pre
              v-if="showSource && programInfo?.source"
              data-testid="program-source"
              class="mt-2 max-h-80 overflow-auto rounded bg-input p-3 text-xs font-mono"
            >{{ programInfo.source }}</pre>
          </dd>

          <dt class="text-faint pt-1">Capabilities</dt>
          <dd>
            <div v-if="!editing" class="flex flex-wrap gap-1.5">
              <span v-for="c in grantedCaps" :key="c" class="meta-chip">{{ c }}</span>
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
            {{ ov.model || 'default' }} / {{ ov.effort || 'default' }}
          </dd>
          <dd v-else class="flex gap-2">
            <input
              v-model="draft.model"
              placeholder="default"
              class="w-28 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
            <input
              v-model="draft.effort"
              placeholder="default"
              class="w-28 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
          </dd>

          <dt class="text-faint">Cooldown</dt>
          <dd v-if="!editing" class="font-mono">{{ ov.cooldown_secs }}s</dd>
          <dd v-else>
            <input
              v-model.number="draft.cooldown"
              type="number"
              min="0"
              class="w-28 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent font-mono"
            />
            <span class="ml-1 text-xs text-faint">seconds</span>
          </dd>

          <dt class="text-faint pt-1">Prompt</dt>
          <dd>
            <p
              v-if="!editing"
              data-testid="overlooker-prompt"
              class="whitespace-pre-wrap text-sm text-fg"
            >
              {{ promptOf(ov) || '— (no prompt; the program decides)' }}
            </p>
            <textarea
              v-else
              v-model="draft.prompt"
              data-testid="overlooker-prompt-input"
              rows="4"
              placeholder="If a session looks stuck, mark it attention and nudge a concrete next step."
              class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent resize-y"
            ></textarea>
          </dd>
        </dl>
      </section>

      <!-- Warm session: the live terminal of the overlooker's persistent
           session when warm mode is on, else a note on how to enable it. -->
      <section
        v-if="ov.warm_session_id"
        class="mb-6"
        data-testid="overlooker-warm-terminal"
      >
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-2">Warm session</h2>
        <AgentTerminal :id="ov.warm_session_id" />
      </section>
      <section v-else class="mb-6 rounded border border-dashed border-line bg-surface p-4">
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1">Warm session</h2>
        <p v-if="ov.warm" class="text-xs text-faint" data-testid="overlooker-warm-pending">
          Warm mode is on. The engine brings up a persistent session on the next
          round; its live terminal appears here, and it carries memory from one
          round to the next.
        </p>
        <p v-else class="text-xs text-faint" data-testid="overlooker-warm-off">
          Each round runs fresh. Turn on warm mode (set <code>params.warm</code>)
          to keep one persistent session with across-round memory — its terminal
          lives here.
        </p>
      </section>

      <!-- Round history — the audit trail. -->
      <section>
        <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-2">
          Round history
          <span class="text-faint font-normal normal-case">({{ runs.length }})</span>
        </h2>

        <p v-if="runsError" class="text-sm text-block">
          Couldn't load round history: {{ runsError }}
        </p>
        <p v-else-if="!runs.length" class="text-sm text-muted">
          No rounds yet. Run one now (or dry-run it) to populate the history.
        </p>

        <ul v-else data-testid="overlooker-runs" class="space-y-2">
          <li
            v-for="r in runs"
            :key="r.id"
            data-testid="overlooker-run-row"
            class="rounded border border-line bg-surface"
          >
            <!-- The row: click anywhere to expand the run's execution log. -->
            <button
              type="button"
              data-testid="overlooker-run-toggle"
              class="w-full p-3 text-left"
              @click="expanded[r.id] = !expanded[r.id]"
            >
              <div class="flex items-center gap-2 flex-wrap">
                <OutcomeBadge :outcome="r.outcome" />
                <span class="text-sm text-muted">{{ r.summary || 'No summary.' }}</span>
                <span class="ml-auto text-xs text-faint">{{ timeAgo(r.started_at) }}</span>
              </div>
              <div class="mt-1 flex items-center gap-2 text-xs text-faint flex-wrap">
                <span class="meta-chip">{{ r.trigger_reason || r.trigger_event || 'manual' }}</span>
                <span v-if="r.duration_ms != null" class="meta-chip">{{ formatMs(r.duration_ms) }}</span>
                <span v-if="r.exit_code != null" class="meta-chip">exit {{ r.exit_code }}</span>
                <span v-if="r.actions && r.actions.length">
                  {{ r.actions.length }} action{{ r.actions.length === 1 ? '' : 's' }}
                </span>
                <span class="ml-auto text-accent">{{ expanded[r.id] ? 'Hide' : 'Details' }}</span>
              </div>
            </button>

            <!-- Expanded: the actions taken, then the captured stdout/stderr —
                 the execution log of exactly what the script printed. -->
            <div
              v-if="expanded[r.id]"
              data-testid="overlooker-run-detail"
              class="border-t border-line p-3 space-y-3"
            >
              <ul
                v-if="r.actions && r.actions.length"
                data-testid="overlooker-run-actions"
                class="space-y-1"
              >
                <li
                  v-for="(a, j) in r.actions"
                  :key="j"
                  class="flex items-start gap-2 text-xs"
                >
                  <span class="meta-chip shrink-0">
                    <span v-if="a.would" class="text-faint">would </span>{{ a.action || a.would || 'action' }}
                    <span v-if="a.level" class="text-faint">={{ a.level }}</span>
                  </span>
                  <span v-if="a.session" class="font-mono text-faint shrink-0">{{ a.session }}</span>
                  <span class="text-muted min-w-0">{{ a.note || a.text || '' }}</span>
                </li>
              </ul>

              <div v-if="r.stdout">
                <h3 class="text-2xs font-semibold uppercase tracking-wider text-faint mb-1">stdout</h3>
                <pre
                  data-testid="overlooker-run-stdout"
                  class="max-h-64 overflow-auto rounded bg-input p-2 text-xs font-mono whitespace-pre-wrap"
                >{{ r.stdout }}</pre>
              </div>
              <div v-if="r.stderr">
                <h3 class="text-2xs font-semibold uppercase tracking-wider text-faint mb-1">stderr</h3>
                <pre
                  data-testid="overlooker-run-stderr"
                  class="max-h-64 overflow-auto rounded bg-input p-2 text-xs font-mono whitespace-pre-wrap text-block"
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
      </section>
    </template>
  </div>
</template>
