<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
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
  validation: [string];
}>();

const limits = ref<ScratchLimits | null>(null);
const fallbackLimits: ScratchLimits = {
  max_files: 20,
  max_file_bytes: 25 * 1024 * 1024,
  max_total_bytes: 50 * 1024 * 1024,
  max_name_bytes: 240,
};
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
  const activeLimits = limits.value ?? fallbackLimits;
  for (const file of files) {
    const name = file.name;
    const nameBytes = new TextEncoder().encode(name).byteLength;
    if (
      !name ||
      name !== name.trim() ||
      name === '.' ||
      name === '..' ||
      name === '.gitignore' ||
      name.includes('/') ||
      name.includes('\\') ||
      /[\u0000-\u001f\u007f-\u009f]/u.test(name) ||
      nameBytes > activeLimits.max_name_bytes
    ) {
      return name === '.gitignore'
        ? "'.gitignore' is reserved for Scratch housekeeping."
        : `${file.name || 'Unnamed file'} must be one control-free file name of at most ${activeLimits.max_name_bytes} UTF-8 bytes.`;
    }
  }
  const next = new Map(props.existing.map((item) => [item.name, item.bytes]));
  for (const file of files) {
    next.set(file.name, file.size);
  }
  for (const [name, bytes] of next) {
    if (bytes > activeLimits.max_file_bytes) {
      return `${name} is ${fmtBytes(bytes)}; the per-file limit is ${fmtBytes(activeLimits.max_file_bytes)}.`;
    }
  }
  if (next.size > activeLimits.max_files) {
    return `Scratch accepts at most ${activeLimits.max_files} files.`;
  }
  const total = [...next.values()].reduce((sum, bytes) => sum + bytes, 0);
  if (total > activeLimits.max_total_bytes) {
    return `Scratch would total ${fmtBytes(total)}; the limit is ${fmtBytes(activeLimits.max_total_bytes)}.`;
  }
  return '';
}

function publishValidation(message: string) {
  error.value = message;
  emit('validation', error.value);
}

function validateCurrent() {
  publishValidation(validate([]));
}

function accept(list: FileList | File[]) {
  if (props.disabled) return;
  const files = Array.from(list);
  if (!files.length) return;
  publishValidation(validate(files));
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
    validateCurrent();
  } catch {
    // Keep the attachment surface usable during a transient observability
    // failure. These values mirror the server constants and still fail closed
    // if the later upload reaches a server with stricter limits.
    limits.value = fallbackLimits;
    validateCurrent();
  }
});

watch(() => props.existing.map((item) => `${item.name}\0${item.bytes}`), validateCurrent);

function resetTransient() {
  dragging.value = false;
  publishValidation('');
  if (fileInput.value) fileInput.value.value = '';
}

defineExpose({ resetTransient });
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
      <input
        ref="fileInput"
        type="file"
        multiple
        class="hidden"
        :disabled="disabled"
        @change="onPick"
      />
    </div>
    <p v-if="error" class="mt-1 text-xs text-block" data-testid="attachment-error" role="alert">
      {{ error }}
    </p>
  </div>
</template>
