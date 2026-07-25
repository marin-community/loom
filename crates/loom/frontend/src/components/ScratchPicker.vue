<script setup lang="ts">
import AttachmentDropzone from './AttachmentDropzone.vue';

// Files staged for a session that doesn't exist yet (the New Session form).
// Unlike ScratchPanel, there's no worktree to upload to — we just collect the
// File objects; the parent base64-encodes them into the create request, which
// drops them into the new worktree's scratch/ before the agent launches.
const files = defineModel<File[]>({ required: true });

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function add(list: File[]) {
  const next = [...files.value];
  for (const f of list) {
    // Same name dropped twice → last one wins, like a real scratch directory.
    const i = next.findIndex((x) => x.name === f.name);
    if (i >= 0) next.splice(i, 1, f);
    else next.push(f);
  }
  files.value = next;
}

function remove(name: string) {
  files.value = files.value.filter((f) => f.name !== name);
}
</script>

<template>
  <div data-testid="scratch-picker">
    <div class="flex items-center justify-between mb-1">
      <label class="text-xs text-muted">Scratch files — optional</label>
      <span class="text-xs text-faint"
        >dropped into <code>scratch/</code>; the agent is told they're there</span
      >
    </div>

    <AttachmentDropzone
      :existing="files.map((file) => ({ name: file.name, bytes: file.size }))"
      test-id="scratch-picker-dropzone"
      @files="add"
    />

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
          class="shrink-0 rounded px-1.5 py-0.5 text-xs text-muted hover:text-block hover:bg-subtle"
          title="Remove"
          @click.stop="remove(f.name)"
        >
          ✕
        </button>
      </li>
    </ul>
  </div>
</template>
