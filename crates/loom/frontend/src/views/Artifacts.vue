<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import type * as Monaco from 'monaco-editor';
import { get, getArtifacts, getArtifact, putArtifact } from '../api';
import type { Session, ArtifactMeta, ArtifactView } from '../types';
import { theme } from '../theme';
import { loadMonaco, monacoTheme, languageForPath } from '../monaco';
import SessionTabs from '../components/SessionTabs.vue';
import SessionPageHeader from '../components/SessionPageHeader.vue';
import MarkdownView from '../components/MarkdownView.vue';

// The Artifacts surface: the agent's out-of-repo documents (designs, reports,
// the `plan`), each named, scoped (branch vs repo-shared), and versioned by
// immutable snapshot. A list on the left, a viewer on the right with a version
// picker and the file browser's proven preview ⇄ Monaco edit toggle — saving an
// edit appends a new revision (`author: user`) via `putArtifact`. Deep-linkable
// at `/s/:id/artifacts/:name`; `artifact_written` over SSE refreshes the list
// and the open viewer. Markdown artifacts render through `MarkdownView` (GFM +
// mermaid + the smartdoc projection: `#N` refs become live status chips).
const props = defineProps<{ id: string; name?: string }>();
const router = useRouter();

// Selecting a work-area tab returns to the session page, carrying which tab so
// Overview lands on Overview (Terminal is the default).
function selectTab(t: 'terminal' | 'overview') {
  router.push(t === 'terminal' ? `/s/${props.id}` : `/s/${props.id}?tab=${t}`);
}

const session = ref<Session | null>(null);
const list = ref<ArtifactMeta[]>([]);
const listError = ref('');
const selected = ref<string>('');

// A header write (rename / acknowledge / archive / adopt) happened — re-fetch
// the session so the shared header reflects it, and refresh the list.
async function reloadSession() {
  try {
    session.value = (await get(`/sessions/${props.id}`)) as Session;
  } catch {
    // Best-effort — the list is the point of this view.
  }
  await loadList();
}

async function loadList() {
  try {
    list.value = await getArtifacts(props.id);
    listError.value = '';
  } catch (e) {
    listError.value = (e as Error).message;
  }
}

// --- Viewer ----------------------------------------------------------------

const view = ref<ArtifactView | null>(null);
const viewRev = ref<number | null>(null); // null = latest
const loading = ref(false);
const viewError = ref('');
const editing = ref(false);
const saving = ref(false);

// Markdown gets the rendered Preview; other kinds show source in Monaco.
const isMarkdown = computed(() => (view.value?.meta.kind ?? 'markdown') === 'markdown');
// The pseudo-path that drives Monaco's language + the markdown image base. The
// artifact name carries no extension, so stamp one on from the kind.
const pseudoPath = computed(() => {
  const name = view.value?.meta.name ?? selected.value;
  return isMarkdown.value ? `${name}.md` : name;
});

type ViewMode = 'preview' | 'source';
const viewMode = ref<ViewMode>('preview');

const host = ref<HTMLElement | null>(null);
let editor: Monaco.editor.IStandaloneCodeEditor | null = null;
let model: Monaco.editor.ITextModel | null = null;

function teardownEditor() {
  editor?.dispose();
  editor = null;
  model?.dispose();
  model = null;
}

async function mountEditor(content: string, readOnly: boolean) {
  const monaco = await loadMonaco();
  model?.dispose();
  model = monaco.editor.createModel(content, languageForPath(monaco, pseudoPath.value));
  if (!editor) {
    editor = monaco.editor.create(host.value!, {
      readOnly,
      automaticLayout: true,
      theme: monacoTheme(theme.value === 'dark'),
      fontSize: 13,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      wordWrap: 'on',
    });
  }
  editor.updateOptions({ readOnly });
  editor.setModel(model);
  monaco.editor.setTheme(monacoTheme(theme.value === 'dark'));
}

// Load an artifact (optionally a specific revision) into the viewer.
async function openArtifact(name: string, rev?: number) {
  selected.value = name;
  viewRev.value = rev ?? null;
  editing.value = false;
  loading.value = true;
  viewError.value = '';
  // Keep the URL in step so the view is deep-linkable / refresh-stable.
  const target = `/s/${props.id}/artifacts/${encodeURIComponent(name)}`;
  if (router.currentRoute.value.path !== target) router.replace(target);
  try {
    view.value = await getArtifact(props.id, name, rev);
    viewMode.value = isMarkdown.value ? 'preview' : 'source';
    await renderViewer();
  } catch (e) {
    viewError.value = (e as Error).message;
    view.value = null;
    teardownEditor();
  } finally {
    loading.value = false;
  }
}

// Show the current view in the chosen mode (Monaco for source / edit, the
// component for preview).
async function renderViewer() {
  if (!view.value) return;
  if (viewMode.value === 'source' && !editing.value) {
    await nextTick();
    await mountEditor(view.value.content, true);
  } else if (!editing.value) {
    // Preview is the MarkdownView component — drop any Monaco model.
    teardownEditor();
  }
}

function setMode(m: ViewMode) {
  if (viewMode.value === m || editing.value) return;
  viewMode.value = m;
  renderViewer();
}

// The version picker: selecting a revision re-fetches at that rev. The latest
// rev is the default; older revs are read-only history (edits always append from
// latest).
function selectRev(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  openArtifact(selected.value, v ? Number(v) : undefined);
}

const onLatest = computed(
  () => !view.value || viewRev.value == null || viewRev.value === view.value.meta.rev,
);

// --- Edit (Monaco) ---------------------------------------------------------

async function edit() {
  if (!view.value) return;
  editing.value = true;
  viewMode.value = 'source';
  await nextTick();
  await mountEditor(view.value.content, false);
  editor?.focus();
}

function cancelEdit() {
  editing.value = false;
  renderViewer();
}

async function save() {
  if (!view.value || !editor) return;
  const content = editor.getValue();
  saving.value = true;
  viewError.value = '';
  try {
    // Append a new revision (author: user); the response is the refreshed view
    // at the new latest rev.
    view.value = await putArtifact(props.id, selected.value, { content });
    viewRev.value = null;
    editing.value = false;
    viewMode.value = isMarkdown.value ? 'preview' : 'source';
    await loadList();
    await renderViewer();
  } catch (e) {
    viewError.value = (e as Error).message;
  } finally {
    saving.value = false;
  }
}

// --- Scope badge -----------------------------------------------------------

function scopeBadge(a: ArtifactMeta): { label: string; title: string } {
  return a.branch_id == null
    ? { label: 'shared', title: 'Repo-shared — visible to every branch in the repo' }
    : { label: 'branch', title: 'Branch-scoped to this session' };
}

// --- SSE-driven refresh ----------------------------------------------------

let source: EventSource | null = null;
function openStream() {
  source = new EventSource(`/api/sessions/${props.id}/events`);
  source.addEventListener('artifact_written', (e) => {
    const d = JSON.parse((e as MessageEvent).data).data as { name?: string };
    loadList().catch(() => {});
    // If the artifact that just changed is the one we're viewing (and we're not
    // mid-edit), refresh the viewer to its new latest.
    if (d?.name && d.name === selected.value && !editing.value) {
      openArtifact(selected.value).catch(() => {});
    }
  });
}

watch(theme, () => {
  if (editor) loadMonaco().then((m) => m.editor.setTheme(monacoTheme(theme.value === 'dark')));
});

// A deep-link name change (router navigation within the view) re-opens.
watch(
  () => props.name,
  (name) => {
    if (name && name !== selected.value) openArtifact(name);
  },
);

onMounted(async () => {
  try {
    session.value = (await get(`/sessions/${props.id}`)) as Session;
  } catch {
    // Header detail is best-effort; the list is the point.
  }
  await loadList();
  openStream();
  // Open the deep-linked artifact, else the well-known `plan`, else the first.
  const want =
    props.name ||
    (list.value.some((a) => a.name === 'plan') ? 'plan' : list.value[0]?.name);
  if (want) openArtifact(want);
});

onUnmounted(() => {
  source?.close();
  teardownEditor();
});
</script>

<template>
  <div class="flex min-h-[28rem] flex-1 flex-col px-5 py-3">
    <SessionPageHeader v-if="session" :ws="session" @reload="reloadSession" />
    <SessionTabs :tab="'artifacts'" :id="props.id" :issue-count="0" @select="selectTab" />

    <p v-if="listError" class="mb-3 text-sm text-block">{{ listError }}</p>

    <!-- Two-pane body: artifact list + viewer. -->
    <div class="flex min-h-0 flex-1 gap-3 rounded border border-line bg-surface overflow-hidden">
      <!-- List -->
      <div class="flex w-72 shrink-0 flex-col border-r border-line">
        <div class="border-b border-line px-2 py-1.5 text-[11px] font-medium uppercase tracking-wide text-faint">
          Artifacts <span class="text-faint">({{ list.length }})</span>
        </div>
        <div class="min-h-0 flex-1 overflow-auto py-1 text-sm">
          <button
            v-for="a in list"
            :key="a.id"
            type="button"
            class="flex w-full flex-col gap-0.5 px-3 py-1.5 text-left hover:bg-subtle/60"
            :class="selected === a.name ? 'bg-subtle' : ''"
            :data-artifact="a.name"
            @click="openArtifact(a.name)"
          >
            <span class="flex items-center gap-1.5">
              <span class="min-w-0 truncate font-mono text-xs" :class="selected === a.name ? 'text-fg' : 'text-muted'">
                {{ a.name }}
              </span>
              <span class="pill ml-auto shrink-0" :title="scopeBadge(a).title">{{ scopeBadge(a).label }}</span>
              <span class="shrink-0 font-mono text-2xs text-faint">v{{ a.rev }}</span>
            </span>
            <span v-if="a.title" class="truncate text-xs text-faint">{{ a.title }}</span>
          </button>
          <p v-if="!list.length && !listError" class="px-3 py-2 text-xs text-faint">
            No artifacts yet. Agents write them with <code>weaver artifact write</code>.
          </p>
        </div>
      </div>

      <!-- Viewer -->
      <div class="flex min-w-0 flex-1 flex-col">
        <!-- Toolbar -->
        <div class="flex flex-wrap items-center gap-3 border-b border-line px-3 py-1.5 text-xs">
          <span class="truncate font-medium text-fg">
            {{ view?.meta.title || selected || 'No artifact selected' }}
          </span>

          <template v-if="view">
            <!-- Version picker -->
            <label class="flex items-center gap-1 text-muted">
              <span class="text-faint">rev</span>
              <select
                class="rounded border border-line bg-surface px-1.5 py-0.5 text-xs text-fg"
                data-testid="artifact-rev"
                :value="viewRev == null ? '' : String(viewRev)"
                :disabled="editing"
                @change="selectRev"
              >
                <option value="">latest (v{{ view.meta.rev }})</option>
                <option v-for="ver in view.versions" :key="ver.rev" :value="String(ver.rev)">
                  v{{ ver.rev }} · {{ ver.author }} · {{ ver.created_at.slice(0, 10) }}
                </option>
              </select>
            </label>

            <div class="ml-auto flex items-center gap-2">
              <!-- Preview ⇄ Source toggle (markdown only; hidden while editing). -->
              <div
                v-if="isMarkdown && !editing"
                class="flex items-center overflow-hidden rounded border border-line"
              >
                <button
                  v-for="m in (['preview', 'source'] as const)"
                  :key="m"
                  class="px-2 py-0.5"
                  :class="viewMode === m ? 'bg-subtle text-fg' : 'text-muted hover:bg-subtle/60'"
                  @click="setMode(m)"
                >
                  {{ m === 'preview' ? 'Preview' : 'Source' }}
                </button>
              </div>

              <template v-if="!editing">
                <button class="btn-secondary px-2.5 py-1 text-xs" :disabled="!onLatest" @click="edit">
                  Edit
                </button>
              </template>
              <template v-else>
                <button class="btn-primary px-2.5 py-1 text-xs" :disabled="saving" @click="save">
                  {{ saving ? 'Saving…' : 'Save' }}
                </button>
                <button class="btn-secondary px-2.5 py-1 text-xs" :disabled="saving" @click="cancelEdit">
                  Cancel
                </button>
              </template>
            </div>
          </template>
        </div>

        <p v-if="!onLatest && !editing" class="border-b border-line bg-subtle/40 px-3 py-1 text-xs text-faint">
          Viewing an older revision — Edit always starts from the latest.
        </p>

        <!-- Content -->
        <div class="relative min-h-0 flex-1 bg-code">
          <!-- Monaco host: always mounted (v-show) so its ref survives mode flips. -->
          <div v-show="(viewMode === 'source' || editing) && view" ref="host" class="h-full w-full"></div>

          <MarkdownView
            v-if="view && isMarkdown && viewMode === 'preview' && !editing"
            :id="props.id"
            :path="pseudoPath"
            :source="view.content"
            :refs="view.refs.issues"
            class="h-full w-full"
          />

          <div
            v-if="!view && !loading"
            class="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-faint"
          >
            <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor"
              stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
              class="opacity-60">
              <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
              <path d="M14 2v4a2 2 0 0 0 2 2h4" />
            </svg>
            <p>{{ viewError || 'Pick an artifact from the list.' }}</p>
          </div>

          <div
            v-if="loading"
            class="absolute right-3 top-2 rounded bg-input/90 px-2 py-1 text-xs text-muted"
          >
            loading…
          </div>
          <p
            v-if="viewError && view"
            class="absolute inset-x-3 top-2 rounded border border-block-line bg-block-soft p-2 text-xs text-block"
          >
            {{ viewError }}
          </p>
        </div>
      </div>
    </div>
  </div>
</template>
