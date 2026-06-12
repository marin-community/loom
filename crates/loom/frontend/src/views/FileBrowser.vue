<script setup lang="ts">
import { ref, reactive, computed, watch, onMounted, onUnmounted } from 'vue';
import type * as Monaco from 'monaco-editor';
import { get } from '../api';
import type { Session, FileTree, FileContent } from '../types';
import { theme } from '../theme';
import { loadMonaco, monacoTheme, languageForPath } from '../monaco';
import { useRouter } from 'vue-router';
import SessionTabs from '../components/SessionTabs.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import MarkdownView from '../components/MarkdownView.vue';

const props = defineProps<{ id: string }>();
const router = useRouter();

// Selecting a work-area tab from the file browser returns to the session page,
// carrying which tab so Overview lands on Overview (Terminal is the default).
function selectTab(t: 'terminal' | 'overview') {
  router.push(t === 'terminal' ? `/s/${props.id}` : `/s/${props.id}?tab=${t}`);
}

// A header write (rename / acknowledge / archive / adopt) happened — re-fetch
// the session so the shared header reflects it, and refresh the tree in case the
// worktree changed underneath us.
async function reloadSession() {
  try {
    session.value = (await get(`/sessions/${props.id}`)) as Session;
  } catch {
    // Best-effort — the tree is still the point of this view.
  }
  await loadTree();
}

// ---------------------------------------------------------------------------
// Tree model — a flat path list from the API assembled into a folder tree.
// ---------------------------------------------------------------------------

interface Node {
  name: string;
  path: string;
  dir: boolean;
  children: Node[];
}

const tree = ref<FileTree | null>(null);
const expanded = reactive(new Set<string>());
const search = ref('');
const selected = ref('');
const loadError = ref('');
const showChanges = ref(true);
const showFiles = ref(true);

// Which baseline "changed" is measured against. `branch` (default) is the
// branch's fork point — everything it introduced; `uncommitted` is just what
// hasn't been committed yet (vs HEAD). The server resolves the actual ref (and
// for `branch` picks the up-to-date base so a stale local mainline doesn't drag
// unrelated upstream commits into the diff).
type DiffBase = 'branch' | 'uncommitted';
const baseOptions: { value: DiffBase; label: string; title: string }[] = [
  {
    value: 'branch',
    label: 'Branch',
    title: 'Everything this branch changed since it forked from its base',
  },
  {
    value: 'uncommitted',
    label: 'Uncommitted',
    title: "Only changes that haven't been committed yet (vs HEAD)",
  },
];
const diffBase = ref<DiffBase>('branch');

const session = ref<Session | null>(null);

function buildTree(t: FileTree): Node {
  const root: Node = { name: '', path: '', dir: true, children: [] };
  // Deleted files are absent from the worktree listing but should stay
  // browsable so their removal can be diffed — fold them back in.
  const paths = new Set<string>(t.files);
  for (const p of Object.keys(t.changed)) paths.add(p);

  for (const full of [...paths].sort()) {
    const parts = full.split('/');
    let cur = root;
    let acc = '';
    parts.forEach((part, i) => {
      acc = acc ? `${acc}/${part}` : part;
      const isDir = i < parts.length - 1;
      let child = cur.children.find((c) => c.name === part && c.dir === isDir);
      if (!child) {
        child = { name: part, path: acc, dir: isDir, children: [] };
        cur.children.push(child);
      }
      cur = child;
    });
  }
  sortNode(root);
  return root;
}

function sortNode(node: Node) {
  node.children.sort((a, b) => {
    if (a.dir !== b.dir) return a.dir ? -1 : 1; // folders first
    return a.name.localeCompare(b.name);
  });
  node.children.forEach(sortNode);
}

const root = computed(() => (tree.value ? buildTree(tree.value) : null));

// Visible rows: a depth-tagged pre-order walk honouring `expanded`. While a
// search is active, only matching files (and their ancestors) show, all expanded.
interface Row {
  node: Node;
  depth: number;
}

function subtreeMatches(node: Node, q: string): boolean {
  if (!node.dir) return node.path.toLowerCase().includes(q);
  return node.children.some((c) => subtreeMatches(c, q));
}

const rows = computed<Row[]>(() => {
  const out: Row[] = [];
  if (!root.value) return out;
  const q = search.value.trim().toLowerCase();
  const walk = (node: Node, depth: number) => {
    for (const child of node.children) {
      if (q && !subtreeMatches(child, q)) continue;
      out.push({ node: child, depth });
      if (child.dir && (q || expanded.has(child.path))) walk(child, depth + 1);
    }
  };
  walk(root.value, 0);
  return out;
});

function toggle(node: Node) {
  if (expanded.has(node.path)) expanded.delete(node.path);
  else expanded.add(node.path);
}

function statusOf(path: string): string | undefined {
  return tree.value?.changed[path];
}

// Just the changed files, flat and sorted, for the pinned Changes list — the
// review surface, reachable without hunting through the full tree. Honours the
// same filter box as the tree so typing narrows both.
const changedList = computed(() => {
  const q = search.value.trim().toLowerCase();
  return Object.entries(tree.value?.changed ?? {})
    .filter(([path]) => !q || path.toLowerCase().includes(q))
    .map(([path, status]) => ({ path, status, name: path.slice(path.lastIndexOf('/') + 1) }))
    .sort((a, b) => a.path.localeCompare(b.path));
});

// Status → single-letter badge + colour class.
function badge(status: string): { letter: string; cls: string } {
  switch (status) {
    // Conventional diff colours, but routed through loom's semantic tokens so
    // they theme light/dark with the rest of the UI. (Added keeps a literal
    // green — the palette has no positive/added token.)
    case 'added':
      return { letter: 'A', cls: 'text-green-600 dark:text-green-400' };
    case 'deleted':
      return { letter: 'D', cls: 'text-block' };
    case 'renamed':
      return { letter: 'R', cls: 'text-accent' };
    case 'copied':
      return { letter: 'C', cls: 'text-accent' };
    default:
      return { letter: 'M', cls: 'text-attn' };
  }
}

// ---------------------------------------------------------------------------
// Viewer — Monaco editor / diff editor / image, driven by the selected file.
// ---------------------------------------------------------------------------

const IMAGE_EXTS = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'svg', 'bmp', 'ico',
]);

type Kind = 'none' | 'text' | 'markdown' | 'image' | 'binary' | 'toolarge' | 'error';
// How the selected file is shown: rendered markdown 'preview', Monaco 'source',
// or the Monaco 'diff' against the base. Markdown files default to 'preview';
// changed files to 'diff'; everything else to 'source'.
type ViewMode = 'preview' | 'source' | 'diff';
const kind = ref<Kind>('none');
const viewMode = ref<ViewMode>('source');
const sideBySide = ref(true);
const loading = ref(false);
const viewError = ref('');
const fileBytes = ref(0);
const mdSource = ref('');

// Extensions that get the rendered-markdown Preview mode.
const MD_EXTS = new Set(['md', 'markdown', 'mdown', 'mkd', 'mkdn', 'mdwn']);
function isMarkdown(path: string): boolean {
  return MD_EXTS.has(extOf(path));
}

const host = ref<HTMLElement | null>(null);

// Monaco handles, kept outside Vue's reactivity (they are not data).
let editor: Monaco.editor.IStandaloneCodeEditor | null = null;
let diffEditor: Monaco.editor.IStandaloneDiffEditor | null = null;
let models: Monaco.editor.ITextModel[] = [];

function extOf(path: string): string {
  const name = path.split('/').pop() ?? '';
  const dot = name.lastIndexOf('.');
  return dot >= 0 ? name.slice(dot + 1).toLowerCase() : '';
}

function rawUrl(path: string): string {
  return `/api/sessions/${props.id}/raw?path=${encodeURIComponent(path)}`;
}

async function getFile(path: string, ref?: 'base'): Promise<FileContent> {
  const params = new URLSearchParams({ path });
  if (ref === 'base') {
    params.set('ref', 'base');
    params.set('base', diffBase.value);
  }
  return (await get(`/sessions/${props.id}/file?${params.toString()}`)) as FileContent;
}

function disposeModels() {
  for (const m of models) m.dispose();
  models = [];
}

function teardownEditors() {
  editor?.dispose();
  editor = null;
  diffEditor?.dispose();
  diffEditor = null;
  disposeModels();
}

const viewerOptions = (): Monaco.editor.IStandaloneEditorConstructionOptions => ({
  readOnly: true,
  automaticLayout: true,
  theme: monacoTheme(theme.value === 'dark'),
  fontSize: 13,
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
  minimap: { enabled: true },
  scrollBeyondLastLine: false,
  renderWhitespace: 'selection',
  smoothScrolling: true,
});

async function mountView(path: string, content: string) {
  const monaco = await loadMonaco();
  if (diffEditor) {
    diffEditor.dispose();
    diffEditor = null;
  }
  disposeModels();
  const model = monaco.editor.createModel(content, languageForPath(monaco, path));
  models.push(model);
  if (!editor) editor = monaco.editor.create(host.value!, viewerOptions());
  editor.setModel(model);
  monaco.editor.setTheme(monacoTheme(theme.value === 'dark'));
}

async function mountDiff(path: string, original: string, modified: string) {
  const monaco = await loadMonaco();
  if (editor) {
    editor.dispose();
    editor = null;
  }
  disposeModels();
  const lang = languageForPath(monaco, path);
  const o = monaco.editor.createModel(original, lang);
  const m = monaco.editor.createModel(modified, lang);
  models.push(o, m);
  if (!diffEditor) {
    diffEditor = monaco.editor.createDiffEditor(host.value!, {
      ...viewerOptions(),
      minimap: { enabled: false },
      renderSideBySide: sideBySide.value,
    });
  }
  diffEditor.updateOptions({ renderSideBySide: sideBySide.value });
  diffEditor.setModel({ original: o, modified: m });
  monaco.editor.setTheme(monacoTheme(theme.value === 'dark'));
}

async function open(path: string) {
  selected.value = path;
  viewError.value = '';
  if (IMAGE_EXTS.has(extOf(path))) {
    teardownEditors();
    kind.value = 'image';
    return;
  }
  // Markdown opens rendered; other changed files open to their diff; the rest
  // to plain source.
  viewMode.value = isMarkdown(path) ? 'preview' : statusOf(path) ? 'diff' : 'source';
  await render();
}

async function render() {
  if (!selected.value || kind.value === 'image') return;
  loading.value = true;
  viewError.value = '';
  try {
    const path = selected.value;
    if (viewMode.value === 'preview') {
      const res = await getFile(path);
      fileBytes.value = res.bytes;
      if (res.binary || res.too_large) {
        kind.value = res.binary ? 'binary' : 'toolarge';
        teardownEditors();
        return;
      }
      // The rendered preview is its own component, not Monaco.
      teardownEditors();
      mdSource.value = res.content ?? '';
      kind.value = 'markdown';
    } else if (viewMode.value === 'diff') {
      const status = statusOf(path);
      const [base, work] = await Promise.all([
        getFile(path, 'base'),
        status === 'deleted' ? Promise.resolve(null) : getFile(path),
      ]);
      if (work && (work.binary || work.too_large)) {
        fileBytes.value = work.bytes;
        kind.value = work.binary ? 'binary' : 'toolarge';
        teardownEditors();
        return;
      }
      const original = base && !base.binary && !base.too_large ? base.content ?? '' : '';
      const modified = work?.content ?? '';
      await mountDiff(path, original, modified);
      kind.value = 'text';
    } else {
      const res = await getFile(path);
      fileBytes.value = res.bytes;
      if (res.binary) {
        kind.value = 'binary';
        teardownEditors();
        return;
      }
      if (res.too_large) {
        kind.value = 'toolarge';
        teardownEditors();
        return;
      }
      await mountView(path, res.content ?? '');
      kind.value = 'text';
    }
  } catch (e) {
    kind.value = 'error';
    viewError.value = (e as Error).message;
    teardownEditors();
  } finally {
    loading.value = false;
  }
}

// The view-mode buttons offered for the current file: Preview only for
// markdown, Source always, Diff only for changed files.
const modeOptions = computed<ViewMode[]>(() => {
  const path = selected.value;
  if (!path || IMAGE_EXTS.has(extOf(path))) return [];
  const opts: ViewMode[] = [];
  if (isMarkdown(path)) opts.push('preview');
  opts.push('source');
  if (statusOf(path)) opts.push('diff');
  return opts;
});

// Show the toggle only when there's a real choice and the file is actually
// renderable (binary/oversized files just get the raw link).
const showModeToggle = computed(
  () => modeOptions.value.length > 1 && (kind.value === 'text' || kind.value === 'markdown'),
);

function modeLabel(m: ViewMode): string {
  if (m === 'preview') return 'Preview';
  if (m === 'diff') return 'Diff';
  return isMarkdown(selected.value) ? 'Source' : 'File';
}

function setMode(m: ViewMode) {
  if (viewMode.value === m) return;
  viewMode.value = m;
  render();
}

// ---------------------------------------------------------------------------
// Loading & lifecycle
// ---------------------------------------------------------------------------

async function loadTree() {
  try {
    tree.value = (await get(`/sessions/${props.id}/tree?base=${diffBase.value}`)) as FileTree;
    autoExpand();
    loadError.value = '';
  } catch (e) {
    loadError.value = (e as Error).message;
  }
}

// Switch the diff baseline: reload the change set and re-render the open file
// (its diff is now taken against the new base).
async function setDiffBase(v: DiffBase) {
  if (diffBase.value === v) return;
  diffBase.value = v;
  await refresh();
}

// Open folders that contain changes (and every top-level folder) so the tree
// lands on something useful instead of fully collapsed.
function autoExpand() {
  if (!tree.value) return;
  for (const c of root.value?.children ?? []) if (c.dir) expanded.add(c.path);
  for (const p of Object.keys(tree.value.changed)) {
    const parts = p.split('/');
    let acc = '';
    for (let i = 0; i < parts.length - 1; i++) {
      acc = acc ? `${acc}/${parts[i]}` : parts[i];
      expanded.add(acc);
    }
  }
}

async function refresh() {
  await loadTree();
  if (selected.value && !tree.value?.files.includes(selected.value) && !statusOf(selected.value)) {
    // The selected file vanished from the tree; clear the viewer.
    selected.value = '';
    kind.value = 'none';
    teardownEditors();
  } else if (selected.value && kind.value !== 'image') {
    render();
  }
}

watch(theme, () => {
  if (editor || diffEditor) loadMonaco().then((m) => m.editor.setTheme(monacoTheme(theme.value === 'dark')));
});

watch(sideBySide, () => diffEditor?.updateOptions({ renderSideBySide: sideBySide.value }));

onMounted(async () => {
  try {
    session.value = (await get(`/sessions/${props.id}`)) as Session;
  } catch {
    // Header detail is best-effort; the tree is the point.
  }
  await loadTree();
});

onUnmounted(teardownEditors);
</script>

<template>
  <!-- Fills the workbench main area exactly: the shared header + sub-nav stay
       put (so the session's context no longer vanishes on the Files surface)
       while the two-pane browser takes the remaining height. -->
  <div class="flex min-h-[28rem] flex-1 flex-col px-5 py-3">
    <SessionPageHeader v-if="session" :ws="session" @reload="reloadSession" />
    <SessionTabs :tab="'files'" :id="props.id" :issue-count="0" @select="selectTab" />
    <!-- Diff baseline selector — branch-since-fork (default) vs uncommitted-only.
         The title lives in the shared header above; Refresh sits in the tree's
         filter row. This slim row just carries the tree-wide diff baseline. -->
    <div class="mb-3 flex items-center">
      <div class="ml-auto flex items-center overflow-hidden rounded border border-line text-xs">
        <button
          v-for="opt in baseOptions"
          :key="opt.value"
          class="px-2 py-1"
          :class="diffBase === opt.value ? 'bg-subtle text-fg' : 'text-muted hover:bg-subtle/60'"
          :title="opt.title"
          @click="setDiffBase(opt.value)"
        >
          {{ opt.label }}
        </button>
      </div>
    </div>

    <p v-if="loadError" class="mb-3 text-sm text-block">{{ loadError }}</p>

    <!-- Two-pane body -->
    <div class="flex min-h-0 flex-1 gap-3 rounded border border-line bg-surface overflow-hidden">
      <!-- Tree -->
      <div class="flex w-72 shrink-0 flex-col border-r border-line">
        <div class="flex items-center gap-1.5 border-b border-line p-2">
          <input
            v-model="search"
            type="text"
            placeholder="Filter files…"
            class="min-w-0 flex-1 rounded bg-input px-2 py-1 text-xs outline-none"
          />
          <button
            type="button"
            class="shrink-0 rounded bg-subtle px-2 py-1 text-xs text-muted hover:bg-subtle-hover hover:text-fg"
            title="Refresh file tree"
            aria-label="Refresh file tree"
            @click="refresh"
          >
            ⟳
          </button>
        </div>
        <!-- Changes: a flat, pinned list of just the changed files, so the
             review surface is reachable without hunting through the tree.
             Clicking a row opens it in the Monaco diff editor. -->
        <div v-if="changedList.length" class="shrink-0 border-b border-line">
          <button
            class="flex w-full items-center gap-1 px-2 py-1 text-[11px] font-medium uppercase tracking-wide text-faint hover:text-muted"
            @click="showChanges = !showChanges"
          >
            <span class="w-3 shrink-0">{{ showChanges ? '▾' : '▸' }}</span>
            <span>Changes</span>
            <span class="text-faint">({{ changedList.length }})</span>
          </button>
          <div v-show="showChanges" class="max-h-48 overflow-auto pb-1 text-sm">
            <div
              v-for="c in changedList"
              :key="c.path"
              class="flex cursor-pointer items-center gap-1 py-0.5 pl-5 pr-2 hover:bg-subtle/60"
              :class="selected === c.path ? 'bg-subtle' : ''"
              :title="c.path"
              @click="open(c.path)"
            >
              <span class="shrink-0 font-mono text-[10px]" :class="badge(c.status).cls">
                {{ badge(c.status).letter }}
              </span>
              <span class="min-w-0 truncate" :class="selected === c.path ? 'text-fg' : 'text-muted'">
                {{ c.name }}
              </span>
            </div>
          </div>
        </div>

        <!-- All files -->
        <button
          class="flex shrink-0 items-center gap-1 px-2 py-1 text-[11px] font-medium uppercase tracking-wide text-faint hover:text-muted"
          @click="showFiles = !showFiles"
        >
          <span class="w-3 shrink-0">{{ showFiles ? '▾' : '▸' }}</span>
          <span>Files</span>
        </button>
        <div v-show="showFiles" class="min-h-0 flex-1 overflow-auto pb-1 text-sm">
          <div
            v-for="row in rows"
            :key="row.node.path"
            class="flex cursor-pointer items-center gap-1 py-0.5 pr-2 hover:bg-subtle/60"
            :class="selected === row.node.path ? 'bg-subtle' : ''"
            :style="{ paddingLeft: `${row.depth * 12 + 8}px` }"
            @click="row.node.dir ? toggle(row.node) : open(row.node.path)"
          >
            <span v-if="row.node.dir" class="w-3 shrink-0 text-faint">
              {{ expanded.has(row.node.path) || search ? '▾' : '▸' }}
            </span>
            <span v-else class="w-3 shrink-0"></span>
            <span class="shrink-0 text-faint">{{ row.node.dir ? '📁' : '' }}</span>
            <span
              class="truncate"
              :class="[
                row.node.dir ? 'text-fg' : 'text-muted',
                statusOf(row.node.path) ? 'font-medium' : '',
              ]"
            >
              {{ row.node.name }}
            </span>
            <span
              v-if="!row.node.dir && statusOf(row.node.path)"
              class="ml-auto shrink-0 font-mono text-[10px]"
              :class="badge(statusOf(row.node.path)!).cls"
            >
              {{ badge(statusOf(row.node.path)!).letter }}
            </span>
          </div>
          <p v-if="rows.length === 0" class="px-3 py-2 text-xs text-faint">
            {{ search ? 'No matching files.' : 'No files.' }}
          </p>
        </div>
      </div>

      <!-- Viewer -->
      <div class="flex min-w-0 flex-1 flex-col">
        <!-- Toolbar -->
        <div class="flex items-center gap-3 border-b border-line px-3 py-1.5 text-xs">
          <span class="truncate font-mono text-muted">{{ selected || 'No file selected' }}</span>

          <template v-if="showModeToggle">
            <div class="ml-auto flex items-center overflow-hidden rounded border border-line">
              <button
                v-for="m in modeOptions"
                :key="m"
                class="px-2 py-0.5"
                :class="viewMode === m ? 'bg-subtle text-fg' : 'text-muted hover:bg-subtle/60'"
                @click="setMode(m)"
              >
                {{ modeLabel(m) }}
              </button>
            </div>
            <label v-if="viewMode === 'diff'" class="flex items-center gap-1 text-muted">
              <input v-model="sideBySide" type="checkbox" class="accent-accent" />
              Side&#8209;by&#8209;side
            </label>
          </template>

          <a
            v-if="selected"
            :href="rawUrl(selected)"
            target="_blank"
            rel="noopener"
            class="text-muted hover:text-fg"
            :class="showModeToggle ? '' : 'ml-auto'"
          >
            Open raw ↗
          </a>
        </div>

        <!-- Content -->
        <div class="relative min-h-0 flex-1 bg-code">
          <!-- Monaco host is always mounted (v-show) so its ref survives. -->
          <div v-show="kind === 'text'" ref="host" class="h-full w-full"></div>

          <MarkdownView
            v-if="kind === 'markdown'"
            :id="props.id"
            :path="selected"
            :source="mdSource"
            class="h-full w-full"
          />

          <div
            v-if="kind === 'image'"
            class="flex h-full w-full items-center justify-center overflow-auto p-4"
          >
            <img :src="rawUrl(selected)" :alt="selected" class="max-h-full max-w-full object-contain" />
          </div>

          <div
            v-else-if="kind === 'none'"
            class="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-faint"
          >
            <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor"
              stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
              class="opacity-60">
              <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
              <path d="M14 2v4a2 2 0 0 0 2 2h4" />
            </svg>
            <p>Pick a file from the tree<span v-if="changedList.length"> — or review the changed files under <strong>Changes</strong></span>.</p>
          </div>

          <div
            v-else-if="kind === 'binary' || kind === 'toolarge'"
            class="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-faint"
          >
            <p>{{ kind === 'binary' ? 'Binary file — not shown.' : 'File too large to display.' }}</p>
            <p class="font-mono text-xs">{{ (fileBytes / 1024).toFixed(1) }} KB</p>
            <a :href="rawUrl(selected)" target="_blank" rel="noopener" class="text-accent hover:underline">
              Open raw ↗
            </a>
          </div>

          <div
            v-else-if="kind === 'error'"
            class="flex h-full w-full items-center justify-center p-4 text-sm text-block"
          >
            {{ viewError }}
          </div>

          <div
            v-if="loading"
            class="absolute right-3 top-2 rounded bg-input/90 px-2 py-1 text-xs text-muted"
          >
            loading…
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
