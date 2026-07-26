<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, useId, watch } from 'vue';

const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    description: string;
    confirmLabel: string;
    busy?: boolean;
    danger?: boolean;
    confirmDisabled?: boolean;
    error?: string;
  }>(),
  {
    busy: false,
    danger: false,
    confirmDisabled: false,
    error: '',
  },
);

const emit = defineEmits<{ confirm: []; cancel: [] }>();
const dialogId = useId();
const titleId = `${dialogId}-title`;
const descriptionId = `${dialogId}-description`;
const errorId = `${dialogId}-error`;
const panel = ref<HTMLElement | null>(null);
const cancelButton = ref<HTMLButtonElement | null>(null);
let returnFocus: HTMLElement | null = null;

function focusable(): HTMLElement[] {
  return Array.from(
    panel.value?.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
    ) ?? [],
  ).filter((element) => !element.hasAttribute('hidden'));
}

function onKeydown(event: KeyboardEvent) {
  if (!props.open) return;
  if (event.key === 'Escape' && !props.busy) {
    event.preventDefault();
    emit('cancel');
    return;
  }
  if (event.key !== 'Tab') return;
  const items = focusable();
  if (!items.length) {
    event.preventDefault();
    panel.value?.focus();
    return;
  }
  const first = items[0];
  const last = items[items.length - 1];
  if (!panel.value?.contains(document.activeElement)) {
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
    return;
  }
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

watch(
  () => props.open,
  async (open) => {
    if (open) {
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      document.addEventListener('keydown', onKeydown);
      await nextTick();
      cancelButton.value?.focus();
    } else {
      document.removeEventListener('keydown', onKeydown);
      await nextTick();
      returnFocus?.focus();
      returnFocus = null;
    }
  },
);

onBeforeUnmount(() => document.removeEventListener('keydown', onKeydown));
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center bg-canvas/75 p-4"
      data-testid="confirm-dialog-backdrop"
      @mousedown.self="!busy && emit('cancel')"
    >
      <section
        ref="panel"
        role="dialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-describedby="error ? `${descriptionId} ${errorId}` : descriptionId"
        tabindex="-1"
        class="w-full max-w-md rounded-md border border-line bg-surface p-4 shadow-xl"
        data-testid="confirm-dialog"
      >
        <h2 :id="titleId" class="text-base font-semibold text-fg">{{ title }}</h2>
        <p v-if="danger" class="mt-1 text-xs font-semibold text-block">Destructive action</p>
        <p :id="descriptionId" class="mt-2 text-sm text-muted">{{ description }}</p>
        <div v-if="$slots.default" class="mt-4">
          <slot></slot>
        </div>
        <p
          v-if="error"
          :id="errorId"
          class="mt-4 rounded border border-block-line bg-block-soft px-3 py-2 text-sm text-block"
          role="alert"
        >
          {{ error }}
        </p>
        <div class="mt-5 flex justify-end gap-2">
          <button
            ref="cancelButton"
            type="button"
            class="btn-secondary px-3 py-1.5 text-sm"
            :disabled="busy"
            data-testid="confirm-dialog-cancel"
            @click="emit('cancel')"
          >
            Cancel
          </button>
          <button
            type="button"
            :class="danger ? 'btn-danger' : 'btn-primary'"
            class="px-3 py-1.5 text-sm"
            :disabled="busy || confirmDisabled"
            data-testid="confirm-dialog-confirm"
            @click="emit('confirm')"
          >
            {{ busy ? 'Working…' : confirmLabel }}
          </button>
        </div>
      </section>
    </div>
  </Teleport>
</template>
