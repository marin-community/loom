<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from 'vue';
import type { SessionSummary } from '../types';
import { autoArchiveDisabled, lifecycleActions } from '../lib/sessionState';
import { useSessionActions } from '../lib/sessionActions';

const props = defineProps<{ ws: SessionSummary }>();
const emit = defineEmits<{ changed: []; error: [string] }>();

const open = ref(false);
const trigger = ref<HTMLButtonElement>();
const menu = ref<HTMLElement>();
const menuId = `${useId()}-menu`;
const actions = computed(() => lifecycleActions(props.ws));
const keepsSession = computed(() => autoArchiveDisabled(props.ws));

const { busy, error, setAutoArchiveDisabled, run } = useSessionActions(
  () => props.ws.id,
  () => emit('changed'),
);

async function invoke(verb: Parameters<typeof run>[0]) {
  open.value = false;
  await run(verb);
  if (error.value) emit('error', error.value);
}

async function toggleAutoArchive() {
  await setAutoArchiveDisabled(!keepsSession.value);
  open.value = false;
  if (error.value) emit('error', error.value);
}

watch(open, async (isOpen) => {
  if (!isOpen) return;
  await nextTick();
  menu.value?.querySelector<HTMLElement>('button:not([disabled])')?.focus();
});

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return;
  event.preventDefault();
  open.value = false;
  void nextTick(() => trigger.value?.focus());
}
</script>

<template>
  <!-- `relative z-10` lifts the control above the row's stretched-link overlay,
       so clicking ⋯ opens the menu instead of opening the session. -->
  <div class="relative z-10 shrink-0">
    <button
      ref="trigger"
      type="button"
      data-testid="row-actions"
      :aria-label="`Actions for ${ws.branch.title || ws.branch.name}`"
      :aria-expanded="open"
      :aria-controls="menuId"
      :class="[
        'rounded px-1.5 py-0.5 text-sm leading-none text-faint transition-colors',
        'hover:bg-subtle hover:text-fg focus-visible:opacity-100',
        open ? 'bg-subtle text-fg opacity-100' : 'opacity-0 group-hover:opacity-100',
      ]"
      @click="open = !open"
    >
      ⋯
    </button>

    <!-- Transparent backdrop dismisses on outside click — the same
         dependency-free pattern as the header's manage popover. -->
    <div v-if="open" class="fixed inset-0 z-20" @click="open = false"></div>
    <div
      v-if="open"
      :id="menuId"
      ref="menu"
      data-testid="row-actions-menu"
      class="absolute right-0 top-full z-30 mt-1 w-64 overflow-hidden rounded border border-line bg-surface py-1 shadow-lg"
      @keydown="onKeydown"
    >
      <button
        v-if="ws.status !== 'archived'"
        type="button"
        data-testid="row-action-auto-archive"
        :disabled="!!busy"
        class="block w-full border-b border-line px-3 py-1.5 text-left text-fg transition-colors hover:bg-subtle disabled:opacity-60"
        @click="toggleAutoArchive"
      >
        <span class="block text-xs font-medium">
          {{
            busy === 'auto-archive'
              ? 'Saving…'
              : keepsSession
                ? 'Enable auto-archive'
                : 'Disable auto-archive'
          }}
        </span>
        <span class="block text-2xs text-faint">
          {{
            keepsSession
              ? 'Allow automatic cleanup again.'
              : 'Keep this session until you archive it.'
          }}
        </span>
      </button>
      <button
        v-for="a in actions"
        :key="a.verb"
        type="button"
        :data-testid="`row-action-${a.verb}`"
        :disabled="!!busy"
        class="block w-full px-3 py-1.5 text-left transition-colors disabled:opacity-60"
        :class="a.danger ? 'text-block hover:bg-block-soft' : 'text-fg hover:bg-subtle'"
        @click="invoke(a.verb)"
      >
        <span class="block text-xs font-medium">
          {{ busy === a.verb ? a.busyLabel : a.label }}
        </span>
        <span class="block text-2xs text-faint">{{ a.hint }}</span>
      </button>
    </div>
  </div>
</template>
