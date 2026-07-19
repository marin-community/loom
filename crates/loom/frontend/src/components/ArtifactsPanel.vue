<script setup lang="ts">
import {
  ref,
  computed,
  watch,
  onMounted,
  onActivated,
  onDeactivated,
  onUnmounted,
  nextTick,
} from 'vue';
import { useRouter } from 'vue-router';
import { getArtifacts, getArtifact, putArtifact, deleteArtifact } from '../api';
import type { ArtifactMeta, ArtifactView } from '../types';
import ArtifactDocument from './ArtifactDocument.vue';
import HtmlArtifactView from './HtmlArtifactView.vue';

// The artifacts surface, as a self-contained panel: a list of the agent's
// out-of-repo documents (designs, reports, the `plan`) on the left, a viewer on
// the right with a version picker and a preview/source edit toggle — saving an
// edit appends a new revision (`author: user`). Markdown
// renders through `ArtifactDocument` (GFM + mermaid + smartdoc chips + the inline
// comment layer); an
// `html` artifact renders as a live document in a sandboxed iframe.
//
// The host (SessionDetail) places this either as a full-width work-area tab or,
// when popped out, in a resizable rail beside the live terminal — `compact`
// collapses the list into a dropdown so the viewer keeps its width in the rail.
// Deep-linkable at `/s/:id/artifacts/:name`; `artifact_written` over SSE
// refreshes the list and the open viewer.
const props = defineProps<{
  id: string;
  /** Deep-linked artifact name to open (from the route). */
  name?: string;
  /** Narrow-rail layout: the list becomes a dropdown above the viewer. */
  compact?: boolean;
  /** Whether the panel is currently docked-out in the rail (drives the toggle). */
  popped?: boolean;
  /** Whether the surface is the one on screen. Kept mounted but hidden (so a
   *  re-open is instant), the panel must not own the route or re-render the
   *  viewer while it is off-screen — `active = false` makes opens silent and
   *  defers a live refresh until it is shown again. Absent ⇒ treated as active
   *  (standalone use). */
  active?: boolean;
}>();
const emit = defineEmits<{ togglePop: []; close: [] }>();

const router = useRouter();

const list = ref<ArtifactMeta[]>([]);
const listError = ref('');
const selected = ref<string>('');

// True once we're the visible surface. Default-on so the panel works standalone.
const isActive = computed(() => props.active !== false);
// An SSE write landed while we were hidden — refresh the open view on return.
const pendingRefresh = ref(false);

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
const removing = ref(false);

const kind = computed(() => view.value?.meta.kind ?? 'markdown');
const isMarkdown = computed(() => kind.value === 'markdown');
const isHtml = computed(() => kind.value === 'html');
// Markdown and HTML both have a rendered Preview ⇄ Source toggle; every other
// kind is shown as read-only source only.
const isRenderable = computed(() => isMarkdown.value || isHtml.value);
// The pseudo-path drives the markdown image base. The artifact name carries no
// extension, so stamp one on from the kind.
const pseudoPath = computed(() => {
  const name = view.value?.meta.name ?? selected.value;
  if (isMarkdown.value) return `${name}.md`;
  if (isHtml.value) return `${name}.html`;
  return name;
});

type ViewMode = 'preview' | 'source';
const viewMode = ref<ViewMode>('preview');
const draftContent = ref('');
const sourceInput = ref<HTMLTextAreaElement | null>(null);

// --- Inline comments ---------------------------------------------------------
// The collaborative markdown surface (ArtifactDocument) renders the artifact and
// owns its inline comment layer. We hold a ref only to forward the shared
// session SSE (`comment_added` / `comment_resolved`) into it. `showComments`
// gates the markdown-preview surface: Source, editing, and non-markdown kinds
// have no comment layer.
const commentsRef = ref<InstanceType<typeof ArtifactDocument> | null>(null);
const showComments = computed(
  () => !!view.value && isMarkdown.value && viewMode.value === 'preview' && !editing.value,
);

// Load an artifact (optionally a specific revision) into the viewer. `keepMode`
// refreshes content without resetting the preview/source choice — for a live
// re-fetch (an SSE write to the open artifact), where snapping back to Preview
// would yank the reader out of a Source view they were reading.
async function openArtifact(name: string, rev?: number, opts?: { keepMode?: boolean }) {
  selected.value = name;
  viewRev.value = rev ?? null;
  editing.value = false;
  loading.value = true;
  viewError.value = '';
  // Keep the URL in step so the view is deep-linkable / refresh-stable — but
  // only while we're the surface on screen, so a hidden panel (the user moved
  // back to the terminal) never yanks the route back to artifacts.
  const target = `/s/${props.id}/artifacts/${encodeURIComponent(name)}`;
  if (isActive.value && router.currentRoute.value.path !== target) router.replace(target);
  try {
    view.value = await getArtifact(props.id, name, rev);
    draftContent.value = view.value.content;
    if (!opts?.keepMode) viewMode.value = isRenderable.value ? 'preview' : 'source';
  } catch (e) {
    viewError.value = (e as Error).message;
    view.value = null;
    draftContent.value = '';
  } finally {
    loading.value = false;
  }
}

function setMode(m: ViewMode) {
  if (viewMode.value === m || editing.value) return;
  viewMode.value = m;
}

// The version picker: selecting a revision re-fetches at that rev. The latest
// rev is the default; older revs are read-only history (edits always append from
// latest).
function selectRev(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  openArtifact(selected.value, v ? Number(v) : undefined);
}

// The compact list dropdown: switch artifacts from a <select>.
function selectFromDropdown(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  if (v) openArtifact(v);
}

const onLatest = computed(
  () => !view.value || viewRev.value == null || viewRev.value === view.value.meta.rev,
);

// --- Edit ------------------------------------------------------------------

async function edit() {
  if (!view.value) return;
  draftContent.value = view.value.content;
  editing.value = true;
  viewMode.value = 'source';
  await nextTick();
  sourceInput.value?.focus();
}

function cancelEdit() {
  editing.value = false;
  draftContent.value = view.value?.content ?? '';
}

async function save() {
  if (!view.value) return;
  const content = draftContent.value;
  saving.value = true;
  viewError.value = '';
  try {
    // Append a new revision (author: user); the response is the refreshed view
    // at the new latest rev.
    view.value = await putArtifact(props.id, selected.value, { content });
    draftContent.value = view.value.content;
    viewRev.value = null;
    editing.value = false;
    viewMode.value = isRenderable.value ? 'preview' : 'source';
    await loadList();
  } catch (e) {
    viewError.value = (e as Error).message;
  } finally {
    saving.value = false;
  }
}

// --- Delete ----------------------------------------------------------------

// Remove the open artifact (every revision). After it's gone, fall back to the
// next artifact in the list, or the empty state when none remain.
async function remove() {
  if (!view.value || removing.value) return;
  const name = selected.value;
  const count = view.value.versions.length;
  if (
    !confirm(
      `Delete artifact "${name}" and all ${count} revision${count === 1 ? '' : 's'}? ` +
        `This cannot be undone.`,
    )
  )
    return;
  removing.value = true;
  viewError.value = '';
  try {
    await deleteArtifact(props.id, name);
    view.value = null;
    draftContent.value = '';
    await loadList();
    const next = list.value[0]?.name;
    if (next) {
      await openArtifact(next);
    } else {
      selected.value = '';
      router.replace(`/s/${props.id}/artifacts`);
    }
  } catch (e) {
    viewError.value = (e as Error).message;
  } finally {
    removing.value = false;
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
function closeStream() {
  source?.close();
  source = null;
}
function openStream() {
  closeStream();
  source = new EventSource(`/api/sessions/${props.id}/events`);
  source.addEventListener('artifact_written', (e) => {
    const d = JSON.parse((e as MessageEvent).data).data as { name?: string; rev?: number };
    loadList().catch(() => {});
    // If the artifact that just changed is the one we're viewing (and we're not
    // mid-edit), refresh the viewer to its new latest — but only while visible;
    // hidden, we defer the re-render until the panel is shown again.
    if (d?.name && d.name === selected.value && !editing.value) {
      // Ignore a replayed/own event we already reflect (the stream replays
      // recent writes on connect): re-opening would needlessly reset the view —
      // and snap a reader out of Source back to Preview. Only a genuinely newer
      // revision refreshes, and it keeps the current preview/source choice.
      if (d.rev != null && onLatest.value && view.value && d.rev <= view.value.meta.rev) return;
      if (isActive.value)
        openArtifact(selected.value, undefined, { keepMode: true }).catch(() => {});
      else pendingRefresh.value = true;
    }
  });
  source.addEventListener('artifact_deleted', (e) => {
    const d = JSON.parse((e as MessageEvent).data).data as { name?: string };
    loadList().catch(() => {});
    // The open artifact was removed elsewhere (CLI, or another tab) — clear the
    // viewer back to the empty state. Our own delete already advanced the view.
    if (d?.name && d.name === selected.value) {
      view.value = null;
      draftContent.value = '';
      selected.value = '';
    }
  });
  // Inline comments: forward to ArtifactDocument rather than opening a second
  // EventSource (the browser caps how many an origin can hold open).
  source.addEventListener('comment_added', (e) => {
    const d = JSON.parse((e as MessageEvent).data).data as { artifact?: string; thread?: number };
    commentsRef.value?.onCommentEvent('comment_added', d);
  });
  source.addEventListener('comment_resolved', (e) => {
    const d = JSON.parse((e as MessageEvent).data).data as { artifact?: string; thread?: number };
    commentsRef.value?.onCommentEvent('comment_resolved', d);
  });
}

// A deep-link name change (route navigation from the host) re-opens.
watch(
  () => props.name,
  (name) => {
    if (name && name !== selected.value) openArtifact(name);
  },
);

// On becoming the visible surface again: restore the URL to the open artifact
// (so it stays deep-linkable) and apply any refresh that landed while hidden.
watch(isActive, (now) => {
  if (!now || !selected.value) return;
  if (pendingRefresh.value) {
    pendingRefresh.value = false;
    openArtifact(selected.value, viewRev.value ?? undefined, { keepMode: true }).catch(() => {});
  } else {
    const target = `/s/${props.id}/artifacts/${encodeURIComponent(selected.value)}`;
    if (router.currentRoute.value.path !== target) router.replace(target);
  }
});

onMounted(async () => {
  await loadList();
  openStream();
  // Open the deep-linked artifact, else the well-known `plan`, else the first.
  const want =
    props.name || (list.value.some((a) => a.name === 'plan') ? 'plan' : list.value[0]?.name);
  if (want) openArtifact(want);
});

// The panel rides inside SessionDetail's <keep-alive>. Pause the status SSE
// while the page is parked (mirroring SessionDetail) so idle EventSources don't
// stack against the browser's per-origin cap; onMounted owns the first open, so
// guard the return-open on `source`.
onActivated(() => {
  if (source) return;
  openStream();
});
onDeactivated(closeStream);

onUnmounted(() => {
  closeStream();
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col rounded border border-line bg-surface overflow-hidden">
    <p v-if="listError" class="border-b border-line px-3 py-1.5 text-xs text-block">
      {{ listError }}
    </p>

    <div class="flex min-h-0 flex-1" :class="compact ? 'flex-col' : ''">
      <!-- List — a sidebar at full width, a dropdown in the narrow rail. -->
      <div v-if="!compact" class="flex w-72 shrink-0 flex-col border-r border-line">
        <div
          class="border-b border-line px-2 py-1.5 text-[11px] font-medium uppercase tracking-wide text-faint"
        >
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
              <span
                class="min-w-0 truncate font-mono text-xs"
                :class="selected === a.name ? 'text-fg' : 'text-muted'"
              >
                {{ a.name }}
              </span>
              <span class="pill ml-auto shrink-0" :title="scopeBadge(a).title">{{
                scopeBadge(a).label
              }}</span>
              <span class="shrink-0 font-mono text-2xs text-faint">v{{ a.rev }}</span>
            </span>
            <span v-if="a.title" class="truncate text-xs text-faint">{{ a.title }}</span>
          </button>
          <p v-if="!list.length && !listError" class="px-3 py-2 text-xs text-faint">
            No artifacts yet. Agents write them with <code>weaver artifact write</code>.
          </p>
        </div>
      </div>

      <!-- Compact list: a dropdown header for the rail. -->
      <div v-else class="flex shrink-0 items-center gap-2 border-b border-line px-2 py-1.5 text-xs">
        <span class="text-faint">Artifact</span>
        <select
          class="min-w-0 flex-1 rounded border border-line bg-surface px-1.5 py-0.5 text-xs text-fg"
          :value="selected"
          @change="selectFromDropdown"
        >
          <option v-if="!list.length" value="">No artifacts yet</option>
          <option v-for="a in list" :key="a.id" :value="a.name">
            {{ a.name }}{{ a.branch_id == null ? ' · shared' : '' }} · v{{ a.rev }}
          </option>
        </select>
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
          </template>

          <div class="ml-auto flex items-center gap-2">
            <template v-if="view">
              <!-- Preview ⇄ Source toggle (markdown/html; hidden while editing). -->
              <div
                v-if="isRenderable && !editing"
                class="flex items-center overflow-hidden rounded border border-line"
              >
                <button
                  v-for="m in ['preview', 'source'] as const"
                  :key="m"
                  class="px-2 py-0.5"
                  :class="viewMode === m ? 'bg-subtle text-fg' : 'text-muted hover:bg-subtle/60'"
                  @click="setMode(m)"
                >
                  {{ m === 'preview' ? 'Preview' : 'Source' }}
                </button>
              </div>

              <template v-if="!editing">
                <button
                  class="btn-secondary px-2.5 py-1 text-xs"
                  :disabled="!onLatest"
                  @click="edit"
                >
                  Edit
                </button>
                <button
                  class="btn-danger px-2.5 py-1 text-xs"
                  data-testid="artifact-delete"
                  :disabled="removing"
                  @click="remove"
                >
                  {{ removing ? 'Deleting…' : 'Delete' }}
                </button>
              </template>
              <template v-else>
                <button class="btn-primary px-2.5 py-1 text-xs" :disabled="saving" @click="save">
                  {{ saving ? 'Saving…' : 'Save' }}
                </button>
                <button
                  class="btn-secondary px-2.5 py-1 text-xs"
                  :disabled="saving"
                  @click="cancelEdit"
                >
                  Cancel
                </button>
              </template>
            </template>

            <!-- Pop out beside the terminal / dock back into the tab. -->
            <button
              class="rounded border border-line px-1.5 py-1 text-muted hover:bg-subtle hover:text-fg"
              data-testid="artifact-pop"
              :title="popped ? 'Dock back into the tab' : 'Pop out beside the terminal'"
              @click="emit('togglePop')"
            >
              {{ popped ? '⤡ Dock' : '⤢ Pop out' }}
            </button>
            <!-- Close the rail entirely (popped only) — back to the plain page. -->
            <button
              v-if="popped"
              class="rounded border border-line px-1.5 py-1 text-muted hover:bg-subtle hover:text-fg"
              data-testid="artifact-rail-close"
              title="Close the artifact panel"
              aria-label="Close artifact panel"
              @click="emit('close')"
            >
              ✕
            </button>
          </div>
        </div>

        <p
          v-if="!onLatest && !editing"
          class="border-b border-line bg-subtle/40 px-3 py-1 text-xs text-faint"
        >
          Viewing an older revision — Edit always starts from the latest.
        </p>

        <!-- Content -->
        <div class="relative min-h-0 flex-1 bg-code">
          <textarea
            v-if="view && editing"
            ref="sourceInput"
            v-model="draftContent"
            data-testid="artifact-source-editor"
            class="h-full w-full resize-none border-0 bg-code p-4 font-mono text-[13px] leading-5 text-fg outline-none"
            spellcheck="false"
            autocomplete="off"
            autocapitalize="off"
          ></textarea>

          <pre
            v-else-if="view && viewMode === 'source'"
            class="h-full w-full overflow-auto whitespace-pre-wrap break-words p-4 font-mono text-[13px] leading-5 text-fg"
          ><code>{{ view.content }}</code></pre>

          <ArtifactDocument
            v-if="showComments && view"
            ref="commentsRef"
            :key="selected"
            :id="props.id"
            :path="pseudoPath"
            :source="view.content"
            :refs="view.refs.issues"
            :artifact-name="selected"
            :rev="view.meta.rev"
            class="h-full w-full"
          />

          <HtmlArtifactView
            v-if="view && isHtml && viewMode === 'preview' && !editing"
            :content="view.content"
            class="h-full w-full"
          />

          <div
            v-if="!view && !loading"
            class="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-faint"
          >
            <svg
              width="28"
              height="28"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.25"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
              class="opacity-60"
            >
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
