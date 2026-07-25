<script setup lang="ts">
import { ref } from 'vue';

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

function confirm() {
  open.value = false;
  emit('confirm');
}
</script>

<template>
  <span class="inline-flex min-w-0 items-center gap-2">
    <button
      v-if="!open"
      type="button"
      class="btn-secondary px-3 py-1.5 text-xs"
      :disabled="disabled"
      @click="open = true"
    >
      {{ label }}
    </button>
    <template v-else>
      <span class="text-xs text-block">{{ message }}</span>
      <button
        type="button"
        class="rounded bg-block px-2.5 py-1 text-xs text-white"
        :disabled="disabled"
        @click="confirm"
      >
        Confirm
      </button>
      <button type="button" class="btn-secondary px-2.5 py-1 text-xs" @click="open = false">
        Cancel
      </button>
    </template>
  </span>
</template>
