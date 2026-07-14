<script setup lang="ts">
import { computed } from 'vue';
import type { Session } from '../types';
import { remedyAction } from '../lib/sessionState';
import { useSessionActions } from '../lib/sessionActions';

// The one button that unsticks a stuck session: Adopt for an orphaned one
// (terminal gone, worktree intact), Recover for an archived one (torn down,
// branch kept). Renders nothing for a healthy session — there is nothing to fix.
//
// It is deliberately parked against the status badge that announces the problem,
// on every surface that shows one (the fleet-list row and the detail header), so
// the cure travels with the diagnosis instead of hiding behind a menu. The same
// verbs are still in the ⋯ menu for anyone who looks there.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ changed: []; error: [string] }>();

const remedy = computed(() => remedyAction(props.ws));

const { busy, error, run } = useSessionActions(
  () => props.ws.id,
  () => emit('changed'),
);

async function invoke() {
  if (!remedy.value) return;
  await run(remedy.value.verb);
  if (error.value) emit('error', error.value);
}
</script>

<template>
  <!-- `relative z-10` keeps it clickable above a list row's stretched-link
       overlay; it is inert on surfaces that have no such overlay. -->
  <button
    v-if="remedy"
    type="button"
    :data-testid="`remedy-${remedy.verb}`"
    :title="remedy.hint"
    :disabled="!!busy"
    class="relative z-10 shrink-0 rounded bg-subtle px-2 py-0.5 text-2xs font-medium text-accent ring-1 ring-inset ring-accent/30 transition-colors hover:bg-subtle-hover disabled:opacity-60"
    @click="invoke"
  >
    {{ busy ? remedy.busyLabel : remedy.label }}
  </button>
</template>
