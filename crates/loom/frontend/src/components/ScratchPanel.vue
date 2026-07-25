<script setup lang="ts">
import { ref, onMounted, onActivated, onDeactivated, onUnmounted } from 'vue';
import { get, upload, del } from '../api';
import type { ScratchFile } from '../types';
import AttachmentDropzone from './AttachmentDropzone.vue';

// Scratch attachments for a session. Browse and drag/drop share the bounded
// AttachmentDropzone path, so a cached detail view can never consume a drop
// intended for another route.
const props = defineProps<{ id: string }>();

const files = ref<ScratchFile[]>([]);
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

async function uploadFiles(list: File[]) {
  if (busy.value) return;
  busy.value = true;
  error.value = '';
  try {
    for (const file of list) {
      await upload(`/sessions/${props.id}/scratch?name=${encodeURIComponent(file.name)}`, file);
    }
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

function onScratchChanged(event: Event) {
  const changedId = (event as CustomEvent<{ id?: string }>).detail?.id;
  if (changedId === props.id) refresh();
}

async function remove(name: string) {
  try {
    await del(`/sessions/${props.id}/scratch?name=${encodeURIComponent(name)}`);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}

let listening = false;
function activate() {
  if (listening) return;
  listening = true;
  refresh();
  window.addEventListener('loom:scratch-changed', onScratchChanged);
}
function deactivate() {
  if (!listening) return;
  listening = false;
  window.removeEventListener('loom:scratch-changed', onScratchChanged);
}

// SessionDetail is kept alive across navigation. Pause the lightweight custom
// refresh listener while hidden; drag/drop itself remains component-local.
onMounted(activate);
onActivated(activate);
onDeactivated(deactivate);
onUnmounted(deactivate);
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

    <AttachmentDropzone
      class="shrink-0"
      :existing="files"
      :disabled="busy"
      compact
      test-id="scratch-dropzone"
      @files="uploadFiles"
    >
      <template #default="{ dragging: over }">
        <span class="flex items-center gap-1 text-2xs">
          <span aria-hidden="true">↥</span>
          {{ busy ? 'Uploading…' : over ? 'Drop to attach' : 'Attach / drop' }}
          <span v-if="files.length" class="pill">{{ files.length }}</span>
        </span>
      </template>
    </AttachmentDropzone>
  </div>
</template>
