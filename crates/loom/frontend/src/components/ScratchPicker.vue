<script setup lang="ts">
import { ref } from 'vue';

// Files staged for a session that doesn't exist yet (the New Session form).
// Unlike ScratchPanel, there's no worktree to upload to — we just collect the
// File objects; the parent base64-encodes them into the create request, which
// drops them into the new worktree's scratch/ before the agent launches.
const files = defineModel<File[]>({ required: true });
const dragging = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function add(list: FileList | File[]) {
  const next = [...files.value];
  for (const f of Array.from(list)) {
    // Same name dropped twice → last one wins, like a real scratch directory.
    const i = next.findIndex((x) => x.name === f.name);
    if (i >= 0) next.splice(i, 1, f);
    else next.push(f);
  }
  files.value = next;
}

function onDrop(e: DragEvent) {
  dragging.value = false;
  const dropped = e.dataTransfer?.files;
  if (dropped && dropped.length) add(dropped);
}

function onPick(e: Event) {
  const input = e.target as HTMLInputElement;
  if (input.files && input.files.length) add(input.files);
  input.value = '';
}

function remove(name: string) {
  files.value = files.value.filter((f) => f.name !== name);
}
</script>

<template>
  <div data-testid="scratch-picker">
    <div class="flex items-center justify-between mb-1">
      <label class="text-xs text-muted">Scratch files — optional</label>
      <span class="text-xs text-faint">dropped into <code>scratch/</code>; the agent is told they're there</span>
    </div>

    <div
      class="rounded border border-dashed px-3 py-5 text-center text-sm transition-colors cursor-pointer"
      :class="dragging ? 'border-accent bg-accent/10 text-fg' : 'border-line text-muted hover:border-accent'"
      data-testid="scratch-picker-dropzone"
      @dragover.prevent="dragging = true"
      @dragleave.prevent="dragging = false"
      @drop.prevent="onDrop"
      @click="fileInput?.click()"
    >
      Drop reference files here, or click to browse
      <input ref="fileInput" type="file" multiple class="hidden" @change="onPick" />
    </div>

    <ul v-if="files.length" class="mt-2 space-y-1 text-sm">
      <li
        v-for="f in files"
        :key="f.name"
        data-testid="scratch-picker-file"
        class="flex items-center justify-between gap-2 rounded bg-canvas/60 px-2 py-1"
      >
        <span class="min-w-0 flex items-baseline gap-2">
          <span class="truncate font-mono text-xs text-fg">{{ f.name }}</span>
          <span class="shrink-0 text-xs text-faint">{{ fmtBytes(f.size) }}</span>
        </span>
        <button
          type="button"
          class="shrink-0 rounded px-1.5 py-0.5 text-xs text-muted hover:text-red-400 hover:bg-subtle"
          title="Remove"
          @click.stop="remove(f.name)"
        >
          ✕
        </button>
      </li>
    </ul>
  </div>
</template>
