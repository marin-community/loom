<script setup lang="ts">
import { computed, ref, onMounted, onActivated, onDeactivated, onUnmounted } from 'vue';
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
const root = ref<HTMLElement | null>(null);
const menuButton = ref<HTMLButtonElement | null>(null);
const menuOpen = ref(false);

// One attachment is useful at a glance on a wide tab row. More than that is a
// collection, not chrome: keep it in a bounded disclosure. Narrow layouts use
// the disclosure for every non-empty collection so even one long filename
// cannot make the session tabs wrap or grow vertically.
const hasManyFiles = computed(() => files.value.length > 1);

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

async function refresh() {
  try {
    files.value = (await get(`/sessions/${props.id}/scratch`)) as ScratchFile[];
    if (!files.value.length) menuOpen.value = false;
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

function toggleMenu() {
  menuOpen.value = !menuOpen.value;
}

function onPointerDown(event: PointerEvent) {
  if (!menuOpen.value || root.value?.contains(event.target as Node)) return;
  menuOpen.value = false;
}

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape' || !menuOpen.value) return;
  event.preventDefault();
  event.stopImmediatePropagation();
  menuOpen.value = false;
  menuButton.value?.focus();
}

let listening = false;
function activate() {
  if (listening) return;
  listening = true;
  refresh();
  window.addEventListener('loom:scratch-changed', onScratchChanged);
  document.addEventListener('pointerdown', onPointerDown);
  document.addEventListener('keydown', onKeydown, true);
}
function deactivate() {
  if (!listening) return;
  listening = false;
  window.removeEventListener('loom:scratch-changed', onScratchChanged);
  document.removeEventListener('pointerdown', onPointerDown);
  document.removeEventListener('keydown', onKeydown, true);
  menuOpen.value = false;
}

// SessionDetail is kept alive across navigation. Pause the lightweight custom
// refresh listener while hidden; drag/drop itself remains component-local.
onMounted(activate);
onActivated(activate);
onDeactivated(deactivate);
onUnmounted(deactivate);
</script>

<template>
  <div
    ref="root"
    class="relative flex min-w-0 items-center gap-1 text-xs"
    data-testid="scratch-panel"
  >
    <ul
      v-if="files.length && !hasManyFiles"
      class="scratch-inline-files flex min-w-0 items-center gap-1.5"
    >
      <li v-for="f in files" :key="f.name" class="meta-chip max-w-52 text-fg">
        <span class="truncate" :title="f.name">{{ f.name }}</span>
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

    <div
      v-if="files.length"
      class="scratch-menu-wrap shrink-0"
      :class="hasManyFiles && 'scratch-menu-wrap--visible'"
    >
      <button
        ref="menuButton"
        type="button"
        class="flex min-h-7 items-center gap-1 rounded border border-line px-2 text-muted hover:bg-subtle hover:text-fg"
        :aria-expanded="menuOpen"
        :aria-controls="`scratch-menu-${props.id}`"
        aria-haspopup="menu"
        :aria-label="`Scratch files, ${files.length} attached`"
        @click="toggleMenu"
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          aria-hidden="true"
        >
          <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
        </svg>
        <span>Scratch</span>
        <span class="pill">{{ files.length }}</span>
      </button>

      <div
        v-if="menuOpen"
        :id="`scratch-menu-${props.id}`"
        class="scratch-menu absolute right-0 top-[calc(100%+0.35rem)] z-30 w-[min(22rem,calc(100vw-1.5rem))] overflow-hidden rounded-md border border-line bg-surface shadow-lg"
        data-testid="scratch-menu"
        role="menu"
      >
        <div class="flex items-center justify-between border-b border-line px-3 py-2">
          <span class="font-medium text-fg">Scratch files</span>
          <span class="text-2xs text-faint">{{ files.length }} attached</span>
        </div>
        <ul class="max-h-[min(24rem,55vh)] overflow-y-auto p-1">
          <li
            v-for="f in files"
            :key="f.name"
            class="flex min-w-0 items-center gap-2 rounded px-2 py-1.5 hover:bg-subtle"
            role="none"
          >
            <span class="min-w-0 flex-1 truncate font-mono text-2xs text-fg" :title="f.name">
              {{ f.name }}
            </span>
            <span class="shrink-0 text-2xs text-faint">{{ fmtBytes(f.bytes) }}</span>
            <button
              type="button"
              role="menuitem"
              class="grid min-h-7 min-w-7 shrink-0 place-items-center rounded text-faint hover:bg-block-soft hover:text-block"
              :title="`Remove ${f.name}`"
              :aria-label="`Remove ${f.name}`"
              @click="remove(f.name)"
            >
              ✕
            </button>
          </li>
        </ul>
      </div>
    </div>

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
          <span>{{ busy ? 'Uploading…' : over ? 'Drop to attach' : 'Attach' }}</span>
          <span v-if="!busy && !over" class="hidden sm:inline">/ drop</span>
          <span v-if="files.length" class="pill">{{ files.length }}</span>
        </span>
      </template>
    </AttachmentDropzone>
  </div>
</template>

<style scoped>
.scratch-menu-wrap {
  display: none;
  position: relative;
}

.scratch-menu-wrap--visible {
  display: block;
}

@media (max-width: 639px) {
  .scratch-inline-files {
    display: none;
  }

  .scratch-menu-wrap {
    display: block;
    position: static;
  }
}
</style>
