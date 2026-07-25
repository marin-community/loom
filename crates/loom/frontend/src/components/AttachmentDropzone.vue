<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { getScratchLimits } from '../api';
import type { ScratchLimits } from '../types';

interface ExistingAttachment {
  name: string;
  bytes: number;
}

const props = withDefaults(
  defineProps<{
    existing: ExistingAttachment[];
    compact?: boolean;
    disabled?: boolean;
    testId?: string;
  }>(),
  {
    compact: false,
    disabled: false,
    testId: 'attachment-dropzone',
  },
);

const emit = defineEmits<{
  files: [File[]];
}>();

const limits = ref<ScratchLimits | null>(null);
const dragging = ref(false);
const error = ref('');
const fileInput = ref<HTMLInputElement | null>(null);

const limitHint = computed(() => {
  if (!limits.value) return 'Reference files are stored in Scratch.';
  return `${limits.value.max_files} files · ${fmtBytes(limits.value.max_file_bytes)} each · ${fmtBytes(limits.value.max_total_bytes)} total`;
});

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function validate(files: File[]): string {
  if (!limits.value) return '';
  const next = new Map(props.existing.map((item) => [item.name, item.bytes]));
  for (const file of files) {
    if (file.size > limits.value.max_file_bytes) {
      return `${file.name} is ${fmtBytes(file.size)}; the per-file limit is ${fmtBytes(limits.value.max_file_bytes)}.`;
    }
    next.set(file.name, file.size);
  }
  if (next.size > limits.value.max_files) {
    return `Scratch accepts at most ${limits.value.max_files} files.`;
  }
  const total = [...next.values()].reduce((sum, bytes) => sum + bytes, 0);
  if (total > limits.value.max_total_bytes) {
    return `Scratch would total ${fmtBytes(total)}; the limit is ${fmtBytes(limits.value.max_total_bytes)}.`;
  }
  return '';
}

function accept(list: FileList | File[]) {
  if (props.disabled) return;
  const files = Array.from(list);
  if (!files.length) return;
  error.value = validate(files);
  if (!error.value) emit('files', files);
}

function onDrop(event: DragEvent) {
  dragging.value = false;
  const files = event.dataTransfer?.files;
  // Some browsers omit or misreport DataTransfer.types. The payload is the
  // authority: a non-empty FileList is accepted regardless of type metadata.
  if (files?.length) accept(files);
}

function onDragOver(event: DragEvent) {
  if (props.disabled) return;
  event.preventDefault();
  const types = Array.from(event.dataTransfer?.types ?? []);
  dragging.value = !types.length || types.includes('Files');
}

function onPick(event: Event) {
  const input = event.target as HTMLInputElement;
  if (input.files?.length) accept(input.files);
  input.value = '';
}

onMounted(async () => {
  try {
    limits.value = await getScratchLimits();
  } catch (cause) {
    error.value = (cause as Error).message;
  }
});
</script>

<template>
  <div class="min-w-0">
    <div
      :data-testid="testId"
      class="cursor-pointer rounded border border-dashed transition-colors"
      :class="[
        compact ? 'px-2 py-1' : 'px-3 py-5 text-center',
        dragging
          ? 'border-accent bg-accent/10 text-fg'
          : 'border-line text-muted hover:border-accent hover:text-fg',
        disabled && 'cursor-wait opacity-60',
      ]"
      role="button"
      tabindex="0"
      :aria-disabled="disabled"
      @dragenter.prevent="onDragOver"
      @dragover="onDragOver"
      @dragleave.prevent="dragging = false"
      @drop.prevent="onDrop"
      @click="!disabled && fileInput?.click()"
      @keydown.enter.prevent="!disabled && fileInput?.click()"
      @keydown.space.prevent="!disabled && fileInput?.click()"
    >
      <slot :dragging="dragging">
        <span class="text-sm">Drop reference files here, or click to browse</span>
        <span v-if="!compact" class="mt-1 block text-xs text-faint">{{ limitHint }}</span>
      </slot>
      <input ref="fileInput" type="file" multiple class="hidden" @change="onPick" />
    </div>
    <p v-if="error" class="mt-1 text-xs text-block" data-testid="attachment-error">
      {{ error }}
    </p>
  </div>
</template>
