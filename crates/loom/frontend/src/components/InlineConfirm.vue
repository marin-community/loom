<script setup lang="ts">
import { nextTick, ref } from 'vue';

withDefaults(
  defineProps<{
    label: string;
    message: string;
    disabled?: boolean;
  }>(),
  { disabled: false },
);

const emit = defineEmits<{ confirm: [] }>();
const open = ref(false);
const trigger = ref<HTMLButtonElement | null>(null);
const confirmButton = ref<HTMLButtonElement | null>(null);

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

function confirm() {
  open.value = false;
  emit('confirm');
}
</script>

<template>
  <span class="inline-flex min-w-0 items-center gap-2" @keydown.esc.prevent.stop="cancel">
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
        class="rounded bg-block px-2.5 py-1 text-xs text-white"
        :disabled="disabled"
        @click="confirm"
      >
        Confirm
      </button>
      <button type="button" class="btn-secondary px-2.5 py-1 text-xs" @click="cancel">
        Cancel
      </button>
    </template>
  </span>
</template>
