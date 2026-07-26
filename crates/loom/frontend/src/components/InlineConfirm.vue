<script setup lang="ts">
import { nextTick, ref } from 'vue';

const props = withDefaults(
  defineProps<{
    label: string;
    message: string;
    disabled?: boolean;
    confirmLabel?: string;
    action?: () => Promise<void>;
    danger?: boolean;
  }>(),
  { disabled: false, confirmLabel: 'Confirm', action: undefined, danger: false },
);

const emit = defineEmits<{ confirm: [] }>();
const open = ref(false);
const trigger = ref<HTMLButtonElement | null>(null);
const confirmButton = ref<HTMLButtonElement | null>(null);
const busy = ref(false);
const error = ref('');

async function show() {
  open.value = true;
  await nextTick();
  confirmButton.value?.focus();
}

async function cancel() {
  open.value = false;
  await nextTick();
  trigger.value?.focus();
}

async function confirm() {
  if (!props.action) {
    open.value = false;
    emit('confirm');
    return;
  }
  busy.value = true;
  error.value = '';
  try {
    await props.action();
    await cancel();
  } catch (cause) {
    error.value = (cause as Error).message;
    await nextTick();
    confirmButton.value?.focus();
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <span
    class="inline-flex min-w-0 flex-wrap items-center gap-2"
    @keydown.esc.prevent.stop="!busy && cancel()"
  >
    <button
      v-if="!open"
      ref="trigger"
      type="button"
      class="btn-secondary px-3 py-1.5 text-xs"
      :disabled="disabled"
      @click="show"
    >
      {{ label }}
    </button>
    <template v-else>
      <span class="text-xs text-block">{{ message }}</span>
      <button
        ref="confirmButton"
        type="button"
        class="rounded px-2.5 py-1 text-xs text-white"
        :class="danger ? 'bg-block' : 'bg-accent'"
        :disabled="disabled || busy"
        @click="confirm"
      >
        {{ busy ? 'Working…' : confirmLabel }}
      </button>
      <button
        type="button"
        class="btn-secondary px-2.5 py-1 text-xs"
        :disabled="busy"
        @click="cancel"
      >
        Cancel
      </button>
      <span v-if="error" class="w-full text-xs text-block" role="alert">{{ error }}</span>
    </template>
  </span>
</template>
