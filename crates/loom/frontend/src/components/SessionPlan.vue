<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue';
import type * as Monaco from 'monaco-editor';
import { getPlan, syncPlan, writeFile } from '../api';
import type { PlanView, PlanTask, PlanSyncResult } from '../types';
import { loadMonaco, monacoTheme, languageForPath } from '../monaco';
import { theme } from '../theme';
import MarkdownView from './MarkdownView.vue';

// The plan surface on a session's Overview, and the only place the goal is
// shown — a plan is the branch goal grown up. With no plan file the panel
// degrades to the goal (the degenerate plan); a real plan renders read-first
// (design + architecture diagram), a task list whose status is PROJECTED from
// the issue ledger (never the file), a dependency graph, and an explicit Edit
// mode that flips to Monaco. Reconcile diffs the plan against its issues. The
// goal seeds a new plan's Problem section (`weaver plan new`), so the two never
// duplicate: goal-only until a plan exists, then folded into the plan.
const props = defineProps<{ id: string; goal?: string }>();

const plan = ref<PlanView | null>(null);
const loaded = ref(false);
const error = ref('');
const notice = ref('');
const busy = ref('');
const editing = ref(false);
const delta = ref<PlanSyncResult | null>(null);

// Monaco handles live OUTSIDE Vue reactivity (wrapping them in a ref breaks the
// editor). The host stays in the DOM via v-show so the ref survives mode flips.
const host = ref<HTMLElement | null>(null);
let editor: Monaco.editor.IStandaloneCodeEditor | null = null;
let model: Monaco.editor.ITextModel | null = null;

async function load(slug?: string) {
  try {
    plan.value = (await getPlan(props.id, slug)) as PlanView;
    error.value = '';
  } catch (e) {
    const msg = (e as Error).message;
    // A repo with no plan is the empty state, not an error.
    if (/not found/i.test(msg)) plan.value = null;
    else error.value = msg;
  } finally {
    loaded.value = true;
  }
}

async function act(name: string, fn: () => Promise<void>) {
  busy.value = name;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
}

function selectSlug(e: Event) {
  load((e.target as HTMLSelectElement).value);
}

// --- Task status projection ------------------------------------------------

interface Status {
  label: string;
  cls: string;
  glyph: string;
}

function statusOf(t: PlanTask): Status {
  if (t.issue_status === 'closed') return { label: 'done', cls: 'text-accent', glyph: '✓' };
  if (t.issue_status === 'open' && t.claimed_branch)
    return { label: `in progress · ${t.claimed_branch}`, cls: 'text-attn', glyph: '◐' };
  if (t.issue_status === 'open') return { label: 'backlog', cls: 'text-muted', glyph: '○' };
  if (t.exec === 'session' || t.exec === 'issue')
    return { label: 'planned', cls: 'text-faint', glyph: '·' };
  return { label: t.exec, cls: 'text-faint', glyph: '·' };
}

const valueRank: Record<string, number> = { high: 0, med: 1, medium: 1, low: 2 };
const tasksByValue = computed(() =>
  [...(plan.value?.tasks ?? [])].sort((a, b) => {
    const r = (valueRank[a.value] ?? 3) - (valueRank[b.value] ?? 3);
    return r !== 0 ? r : Number(a.id.slice(1)) - Number(b.id.slice(1));
  }),
);

// A mermaid dependency graph, fed to MarkdownView (which renders mermaid). Nodes
// are tasks (label carries a status glyph); edges are `deps`. Only rendered when
// at least one dependency exists.
const graphSource = computed(() => {
  const tasks = plan.value?.tasks ?? [];
  const ids = new Set(tasks.map((t) => t.id));
  const edges = tasks.flatMap((t) => t.deps.filter((d) => ids.has(d)).map((d) => `    ${d} --> ${t.id}`));
  if (edges.length === 0) return '';
  const nodes = tasks.map((t) => {
    const label = `${t.id} · ${t.title} ${statusOf(t).glyph}`.replace(/["[\]]/g, '');
    return `    ${t.id}["${label}"]`;
  });
  return '```mermaid\nflowchart TD\n' + [...nodes, ...edges].join('\n') + '\n```\n';
});

// --- Edit mode (Monaco) ----------------------------------------------------

async function mountEditor() {
  const monaco = await loadMonaco();
  model?.dispose();
  model = monaco.editor.createModel(plan.value!.content, languageForPath(monaco, plan.value!.path));
  if (!editor) {
    editor = monaco.editor.create(host.value!, {
      automaticLayout: true,
      theme: monacoTheme(theme.value === 'dark'),
      fontSize: 13,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      wordWrap: 'on',
    });
  }
  editor.setModel(model);
  monaco.editor.setTheme(monacoTheme(theme.value === 'dark'));
}

function teardownEditor() {
  editor?.dispose();
  editor = null;
  model?.dispose();
  model = null;
}

async function edit() {
  editing.value = true;
  delta.value = null;
  await nextTick();
  await mountEditor();
}

function cancel() {
  editing.value = false;
  teardownEditor();
}

function save() {
  const next = editor?.getValue() ?? '';
  act('save', async () => {
    await writeFile(props.id, plan.value!.path, next);
    await load(plan.value!.slug);
    editing.value = false;
    teardownEditor();
    notice.value = 'Plan saved. Reconcile to update its issues.';
  });
}

// --- Reconcile -------------------------------------------------------------

function preview() {
  act('reconcile', async () => {
    delta.value = (await syncPlan(props.id, plan.value!.slug, false)) as PlanSyncResult;
    if (delta.value.actions.length === 0) {
      notice.value = 'Plan and issues are already in sync.';
      delta.value = null;
    }
  });
}

function applyDelta() {
  act('apply', async () => {
    const res = (await syncPlan(props.id, plan.value!.slug, true)) as PlanSyncResult;
    delta.value = null;
    await load(plan.value!.slug);
    notice.value =
      `Applied ${res.actions.length} change(s)` + (res.flags ? `; ${res.flags} in-flight task(s) flagged.` : '.');
  });
}

function actionLine(a: PlanSyncResult['actions'][number]): string {
  switch (a.kind) {
    case 'create':
      return `+ create issue for ${a.task}: ${a.title}`;
    case 'close':
      return `− close #${a.issue_id} (${a.task} removed from plan)`;
    case 'update_title':
      return `~ retitle #${a.issue_id} (${a.task}) → ${a.title}`;
    case 'flag':
      return `! flag #${a.issue_id} (${a.task} ← ${a.branch}): ${a.reason}`;
  }
}

// Keep an open Monaco editor in step with a live light/dark toggle, matching
// FileBrowser. The markdown preview re-themes itself via its own watcher.
watch(theme, () => {
  if (editor) loadMonaco().then((m) => m.editor.setTheme(monacoTheme(theme.value === 'dark')));
});

onMounted(() => load());
onUnmounted(teardownEditor);
</script>

<template>
  <section class="rounded border border-line bg-surface" data-testid="session-plan">
    <header class="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
      <div class="text-xs font-medium uppercase tracking-wide text-faint">Plan</div>
      <template v-if="plan">
        <span class="text-sm font-medium text-fg">{{ plan.title }}</span>
        <span class="pill">{{ plan.status }}</span>
        <select
          v-if="plan.available.length > 1"
          class="ml-1 rounded border border-line bg-surface px-1.5 py-0.5 text-xs text-fg"
          :value="plan.slug"
          @change="selectSlug"
        >
          <option v-for="s in plan.available" :key="s" :value="s">{{ s }}</option>
        </select>
        <div class="ml-auto flex gap-1.5">
          <template v-if="!editing">
            <button
              class="rounded bg-subtle px-2.5 py-1 text-xs text-fg hover:bg-subtle-hover"
              @click="edit"
            >
              Edit
            </button>
            <button
              class="rounded bg-subtle px-2.5 py-1 text-xs text-accent ring-1 ring-inset ring-accent/30 hover:bg-subtle-hover"
              :disabled="busy === 'reconcile'"
              @click="preview"
            >
              {{ busy === 'reconcile' ? 'Checking…' : 'Reconcile' }}
            </button>
          </template>
          <template v-else>
            <button
              class="rounded bg-accent px-2.5 py-1 text-xs text-accent-fg hover:bg-accent-hover"
              :disabled="busy === 'save'"
              @click="save"
            >
              {{ busy === 'save' ? 'Saving…' : 'Save' }}
            </button>
            <button
              class="rounded bg-subtle px-2.5 py-1 text-xs text-fg hover:bg-subtle-hover"
              @click="cancel"
            >
              Cancel
            </button>
          </template>
        </div>
      </template>
    </header>

    <p v-if="error" class="m-3 rounded border border-block-line bg-block-soft p-2 text-sm text-block">
      {{ error }}
    </p>
    <p v-if="notice" class="mx-4 mt-3 text-sm text-accent">{{ notice }}</p>

    <!-- No plan file yet: the panel degrades to the branch goal — a plan is the
         goal grown up, and `weaver plan new` seeds its Problem from this goal. -->
    <div v-if="loaded && !plan && !error" class="px-4 py-5">
      <p v-if="goal" data-testid="session-goal" class="whitespace-pre-wrap text-sm text-fg">{{ goal }}</p>
      <p v-else class="text-sm text-faint">No goal set.</p>
      <p class="mt-3 text-xs text-faint">
        Scaffold a structured plan with <code>weaver plan new "&lt;title&gt;"</code>
        for multi-session work — it starts from this goal.
      </p>
    </div>

    <!-- Reconcile preview: the delta, applied on confirm. -->
    <div v-if="delta && delta.actions.length" class="m-3 rounded border border-attn-line bg-attn-soft p-3">
      <div class="mb-1.5 text-xs font-medium uppercase tracking-wide text-attn">
        Proposed changes ({{ delta.actions.length }})
      </div>
      <ul class="space-y-0.5 font-mono text-xs text-fg">
        <li v-for="(a, i) in delta.actions" :key="i">{{ actionLine(a) }}</li>
      </ul>
      <div class="mt-2.5 flex gap-1.5">
        <button
          class="rounded bg-accent px-2.5 py-1 text-xs text-accent-fg hover:bg-accent-hover"
          :disabled="busy === 'apply'"
          @click="applyDelta"
        >
          {{ busy === 'apply' ? 'Applying…' : `Apply ${delta.actions.length} change(s)` }}
        </button>
        <button class="rounded bg-subtle px-2.5 py-1 text-xs text-fg hover:bg-subtle-hover" @click="delta = null">
          Dismiss
        </button>
      </div>
    </div>

    <div v-if="plan">
      <!-- Task list — the live projection: status comes from the issue ledger,
           sorted so the highest-value work surfaces first. -->
      <ul v-if="plan.tasks.length" class="divide-y divide-line">
        <li v-for="t in tasksByValue" :key="t.id" class="flex items-baseline gap-2 px-4 py-1.5 text-sm">
          <span class="font-mono text-xs text-faint">{{ t.id }}</span>
          <span :class="statusOf(t).cls" :title="statusOf(t).label">{{ statusOf(t).glyph }}</span>
          <span class="text-fg">{{ t.title }}</span>
          <span v-if="t.value" class="pill">{{ t.value }}</span>
          <span class="ml-auto text-xs" :class="statusOf(t).cls">{{ statusOf(t).label }}</span>
          <span v-if="t.deps.length" class="text-xs text-faint">deps: {{ t.deps.join(', ') }}</span>
        </li>
      </ul>

      <!-- Dependency graph (only when there are edges). -->
      <div v-if="graphSource" class="border-t border-line">
        <MarkdownView :id="props.id" :path="plan.path" :source="graphSource" />
      </div>

      <!-- The plan document: rendered read-first, or Monaco when editing. The
           host stays mounted (v-show) so its ref survives the mode flip. -->
      <div v-show="editing" ref="host" class="h-[60vh] w-full border-t border-line"></div>
      <div v-show="!editing" class="border-t border-line">
        <MarkdownView :id="props.id" :path="plan.path" :source="plan.content" />
      </div>
    </div>
  </section>
</template>
