<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { get, upload, del } from '../api';
import type { ScratchFile } from '../types';

// Scratch attachments for a session, as a slim strip that sits in the working
// zone right under the terminal — drop a file here and reference it from the
// agent as `scratch/<name>`. Deliberately one quiet row (not a tall card): the
// terminal is the surface, this is just the side door for handing the agent a
// file. Files show as removable chips inline.
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
  <section data-testid="scratch-panel">
    <!-- The whole strip is a drop target; the labelled affordance is a real
         <button> so click AND keyboard (Enter/Space) both open the file picker.
         The chips' remove buttons sit as siblings, never nested in another
         control. -->
    <div
      class="flex flex-wrap items-center gap-x-2 gap-y-1 rounded border border-dashed px-2.5 py-1 text-xs transition-colors"
      :class="dragging ? 'border-accent bg-accent/10' : 'border-line'"
      data-testid="scratch-dropzone"
      @dragover.prevent="dragging = true"
      @dragleave.prevent="dragging = false"
      @drop.prevent="onDrop"
    >
      <button
        type="button"
        class="flex cursor-pointer items-center gap-1.5 rounded text-left hover:text-fg focus:outline-none focus-visible:ring-1 focus-visible:ring-accent"
        :class="dragging ? 'text-fg' : 'text-faint'"
        @click="fileInput?.click()"
      >
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
          stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
        </svg>
        <span>{{ busy ? 'Uploading…' : 'Drop a file or click to attach' }}</span>
      </button>
      <span class="font-mono text-2xs text-faint">— reference as <code>scratch/&lt;name&gt;</code></span>

      <ul v-if="files.length" class="ml-auto flex flex-wrap items-center gap-1.5">
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

      <input ref="fileInput" type="file" multiple class="hidden" @change="onPick" />
    </div>

    <p v-if="error" class="mt-1 text-xs text-block">{{ error }}</p>
  </section>
</template>
