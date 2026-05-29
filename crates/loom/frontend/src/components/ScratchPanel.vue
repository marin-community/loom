<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { get, upload, del } from '../api';
import type { ScratchFile } from '../types';

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

function onDrop(e: DragEvent) {
  dragging.value = false;
  const dropped = e.dataTransfer?.files;
  if (dropped && dropped.length) uploadFiles(dropped);
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

onMounted(refresh);
</script>

<template>
  <section class="rounded border border-line bg-surface p-4" data-testid="scratch-panel">
    <div class="flex items-center justify-between mb-2">
      <label class="text-xs text-muted">Scratch files</label>
      <span class="text-xs text-faint">reference as <code>scratch/&lt;name&gt;</code></span>
    </div>

    <div
      class="rounded border border-dashed px-3 py-6 text-center text-sm transition-colors cursor-pointer"
      :class="dragging ? 'border-accent bg-accent/10 text-fg' : 'border-line text-muted hover:border-accent'"
      data-testid="scratch-dropzone"
      @dragover.prevent="dragging = true"
      @dragleave.prevent="dragging = false"
      @drop.prevent="onDrop"
      @click="fileInput?.click()"
    >
      <span v-if="busy">Uploading…</span>
      <span v-else>Drop files here, or click to browse</span>
      <input ref="fileInput" type="file" multiple class="hidden" @change="onPick" />
    </div>

    <p v-if="error" class="mt-2 text-xs text-red-400">{{ error }}</p>

    <ul v-if="files.length" class="mt-3 space-y-1 text-sm">
      <li
        v-for="f in files"
        :key="f.name"
        class="flex items-center justify-between gap-2 rounded bg-canvas/60 px-2 py-1"
      >
        <span class="min-w-0 flex items-baseline gap-2">
          <span class="truncate font-mono text-xs text-fg">{{ f.name }}</span>
          <span class="shrink-0 text-xs text-faint">{{ fmtBytes(f.bytes) }}</span>
        </span>
        <button
          type="button"
          class="shrink-0 rounded px-1.5 py-0.5 text-xs text-muted hover:text-red-400 hover:bg-subtle"
          title="Remove"
          @click="remove(f.name)"
        >
          ✕
        </button>
      </li>
    </ul>
    <p v-else class="mt-3 text-xs text-faint">No scratch files yet.</p>
  </section>
</template>
