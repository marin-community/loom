<script setup lang="ts">
import { computed, ref } from 'vue';
import { del, post } from '../api';
import type { AutomationRun } from '../types';
import { exactTime, timeAgo } from '../lib/time';
import ConfirmDialog from './ConfirmDialog.vue';
import StatusBadge from './StatusBadge.vue';

const props = defineProps<{ run: AutomationRun; intervention: boolean; history?: boolean }>();
const emit = defineEmits<{ changed: []; error: [message: string] }>();
const open = ref(false);
const busy = ref('');
const pendingAction = ref<'archive' | 'remove' | ''>('');
const canArchive = computed(
  () => props.run.status !== 'cancelled' && props.run.status !== 'completed',
);

const title = computed(() => {
  if (props.intervention) return `Launch ${props.run.status}`;
  if (props.history) return `Run ${props.run.status}`;
  return 'Provisioning session';
});

async function act(name: string, fn: () => Promise<void>) {
  open.value = false;
  busy.value = name;
  try {
    await fn();
    emit('changed');
  } catch (error) {
    emit('error', (error as Error).message);
  } finally {
    busy.value = '';
  }
}

function requestConfirmation(action: 'archive' | 'remove') {
  open.value = false;
  pendingAction.value = action;
}

function confirmAction() {
  const action = pendingAction.value;
  pendingAction.value = '';
  if (action === 'archive') {
    void act('archive', async () => {
      await post(`/sessions/${props.run.session_id}/archive`);
    });
  } else if (action === 'remove') {
    void act('remove', async () => {
      await del(`/sessions/${props.run.session_id}`);
    });
  }
}
</script>

<template>
  <li
    class="group relative flex items-start gap-3 border-b border-line px-3 py-2.5 last:border-0"
    data-testid="automation-run-only"
    :data-run-id="run.id"
  >
    <span
      class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
      :class="intervention ? 'bg-block-line' : 'bg-info-line'"
      aria-hidden="true"
    ></span>
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <span class="text-[15px] font-semibold text-fg">
          {{ title }}
        </span>
        <StatusBadge :status="run.status" />
      </div>
      <p v-if="intervention && run.summary" class="mt-0.5 break-words font-mono text-xs text-block">
        {{ run.summary }}
      </p>
    </div>
    <div class="shrink-0 text-right font-mono text-2xs text-faint">
      <div>{{ run.source }} · {{ run.service_tag }}</div>
      <div>{{ run.profile }}</div>
      <time
        :datetime="run.updated_at"
        :title="exactTime(run.updated_at)"
        :aria-label="exactTime(run.updated_at)"
      >
        {{ timeAgo(run.updated_at) }}
      </time>
    </div>
    <div class="relative z-10 shrink-0">
      <button
        type="button"
        data-testid="run-actions"
        :aria-label="`Actions for launch ${run.id}`"
        :aria-expanded="open"
        :disabled="!!busy"
        :class="[
          'rounded px-1.5 py-0.5 text-sm leading-none text-faint transition-colors',
          'hover:bg-subtle hover:text-fg focus-visible:opacity-100 disabled:opacity-50',
          open ? 'bg-subtle text-fg opacity-100' : 'opacity-0 group-hover:opacity-100',
        ]"
        @click="open = !open"
      >
        ⋯
      </button>
      <div v-if="open" class="fixed inset-0 z-20" @click="open = false"></div>
      <div
        v-if="open"
        class="absolute right-0 top-full z-30 mt-1 w-64 overflow-hidden rounded border border-line bg-surface py-1 shadow-lg"
        data-testid="run-actions-menu"
      >
        <button
          v-if="canArchive"
          type="button"
          data-testid="run-action-archive"
          class="block w-full px-3 py-1.5 text-left text-fg transition-colors hover:bg-subtle"
          @click="requestConfirmation('archive')"
        >
          <span class="block text-xs font-medium">Archive</span>
          <span class="block text-2xs text-faint">Tear down runtime, keep launch history</span>
        </button>
        <button
          type="button"
          data-testid="run-action-remove"
          class="block w-full px-3 py-1.5 text-left text-block transition-colors hover:bg-block-soft"
          @click="requestConfirmation('remove')"
        >
          <span class="block text-xs font-medium">Remove</span>
          <span class="block text-2xs text-faint">Delete this attempt and reserved runtime</span>
        </button>
      </div>
    </div>
    <ConfirmDialog
      :open="pendingAction !== ''"
      :title="pendingAction === 'archive' ? 'Archive launch attempt?' : 'Remove launch attempt?'"
      :description="
        pendingAction === 'archive'
          ? 'Any reserved runtime is torn down and the attempt is kept in History.'
          : 'This permanently deletes the attempt and any reserved runtime.'
      "
      :confirm-label="pendingAction === 'archive' ? 'Archive' : 'Remove'"
      :danger="pendingAction === 'remove'"
      :busy="!!busy"
      @confirm="confirmAction"
      @cancel="pendingAction = ''"
    />
  </li>
</template>
