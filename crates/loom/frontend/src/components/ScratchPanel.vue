<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { get, upload, del } from '../api';
import type { ScratchFile } from '../types';

// Scratch attachments for a session — drop a file anywhere on the page (or
// click the paperclip) and reference it from the agent as `scratch/<name>`.
// Renders as a compact strip in the tab row's spare right side rather than its
// own row, so the terminal keeps the vertical space; the drop target is the
// whole window, announced by a full-page overlay while a file drag is over it.
const props = defineProps<{ id: string }>();

const files = ref<ScratchFile[]>([]);
const dragging = ref(false);
const busy = ref(false);
const error = ref('');

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

async function refresh() {
  try {
    files.value = (await get(`/sessions/${props.id}/scratch`)) as ScratchFile[];
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function uploadFiles(list: FileList | File[]) {
  if (busy.value) return;
  busy.value = true;
  error.value = '';
  try {
    for (const file of Array.from(list)) {
      await upload(`/sessions/${props.id}/scratch?name=${encodeURIComponent(file.name)}`, file);
    }
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

// Window-level drag tracking. dragenter/dragleave fire for every element the
// drag crosses, so a depth counter (not a boolean) tells "still over the
// window" from "left it". Only file drags count — text selections dragged
// within the terminal carry no Files type and must not trigger the overlay.
let depth = 0;

function hasFiles(e: DragEvent): boolean {
  return Array.from(e.dataTransfer?.types ?? []).includes('Files');
}

function onDragEnter(e: DragEvent) {
  if (!hasFiles(e)) return;
  depth += 1;
  dragging.value = true;
}

function onDragLeave() {
  // Unlike dragenter, don't gate on hasFiles: some browsers report empty types
  // on dragleave, and bailing then would leave the overlay stuck on. Tracking
  // depth > 0 already implies the drag we're unwinding was a file drag.
  if (depth === 0) return;
  depth -= 1;
  if (depth === 0) dragging.value = false;
}

function onDragOver(e: DragEvent) {
  // preventDefault marks the window as a valid drop target.
  if (hasFiles(e)) e.preventDefault();
}

function onDrop(e: DragEvent) {
  depth = 0;
  dragging.value = false;
  if (!hasFiles(e)) return;
  e.preventDefault();
  const dropped = e.dataTransfer?.files;
  if (dropped && dropped.length) uploadFiles(dropped);
}

function onScratchChanged(event: Event) {
  const changedId = (event as CustomEvent<{ id?: string }>).detail?.id;
  if (changedId === props.id) refresh();
}

const fileInput = ref<HTMLInputElement | null>(null);
function onPick(e: Event) {
  const input = e.target as HTMLInputElement;
  if (input.files && input.files.length) uploadFiles(input.files);
  input.value = '';
}

async function remove(name: string) {
  try {
    await del(`/sessions/${props.id}/scratch?name=${encodeURIComponent(name)}`);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}

onMounted(() => {
  refresh();
  window.addEventListener('dragenter', onDragEnter);
  window.addEventListener('dragleave', onDragLeave);
  window.addEventListener('dragover', onDragOver);
  window.addEventListener('drop', onDrop);
  window.addEventListener('loom:scratch-changed', onScratchChanged);
});
onUnmounted(() => {
  window.removeEventListener('dragenter', onDragEnter);
  window.removeEventListener('dragleave', onDragLeave);
  window.removeEventListener('dragover', onDragOver);
  window.removeEventListener('drop', onDrop);
  window.removeEventListener('loom:scratch-changed', onScratchChanged);
});
</script>

<template>
  <div class="flex min-w-0 items-center gap-1 text-xs" data-testid="scratch-panel">
    <ul v-if="files.length" class="flex min-w-0 flex-wrap items-center gap-1.5">
      <li v-for="f in files" :key="f.name" class="meta-chip text-fg">
        <span class="truncate">{{ f.name }}</span>
        <span class="text-faint">{{ fmtBytes(f.bytes) }}</span>
        <button
          type="button"
          class="text-faint hover:text-block"
          :title="`Remove ${f.name}`"
          :aria-label="`Remove ${f.name}`"
          @click="remove(f.name)"
        >
          ✕
        </button>
      </li>
    </ul>

    <p v-if="error" class="truncate text-block" :title="error">{{ error }}</p>

    <!-- The labelled affordance is a real <button> so click AND keyboard
         (Enter/Space) both open the file picker. -->
    <button
      type="button"
      class="flex shrink-0 cursor-pointer items-center gap-1 rounded px-1.5 py-0.5 text-faint hover:bg-subtle hover:text-fg focus:outline-none focus-visible:ring-1 focus-visible:ring-accent disabled:cursor-wait disabled:opacity-60"
      :title="
        busy
          ? 'Uploading scratch file(s)…'
          : 'Attach a file — the agent sees it as scratch/<name> (or drop one anywhere on the page)'
      "
      :aria-label="busy ? 'Uploading scratch file(s)' : 'Attach a scratch file'"
      :disabled="busy"
      @click="fileInput?.click()"
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path
          d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"
        />
      </svg>
      <span v-if="busy" class="text-2xs text-faint">Uploading…</span>
      <span v-else-if="files.length" class="pill">{{ files.length }}</span>
    </button>
    <input ref="fileInput" type="file" multiple class="hidden" @change="onPick" />

    <!-- Full-page drop announcement while a file drag is over the window. The
         window listeners above own the actual drop; this layer is the cue. -->
    <Teleport to="body">
      <div
        v-if="dragging"
        data-testid="scratch-dropzone"
        class="fixed inset-0 z-40 flex items-center justify-center bg-canvas/70"
      >
        <div
          class="rounded-md border-2 border-dashed border-accent bg-surface px-6 py-4 text-sm text-fg shadow-lg"
        >
          Drop to attach — the agent sees it as
          <code class="font-mono">scratch/&lt;name&gt;</code>
        </div>
      </div>
    </Teleport>
  </div>
</template>
