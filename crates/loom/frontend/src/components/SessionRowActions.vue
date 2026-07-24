<script setup lang="ts">
import { computed, ref } from 'vue';
import type { Session } from '../types';
import { autoArchiveDisabled, lifecycleActions, shelved } from '../lib/sessionState';
import { useSessionActions } from '../lib/sessionActions';

// A fleet-list row's ⋯ menu: every lifecycle verb that applies to the session
// (its remedy, Archive, Remove). Before this the fleet list — the one place you
// actually survey and tidy a fleet — could not act on it at all: adopting or
// archiving meant opening the session and hunting through the header's popover.
//
// Quiet until wanted: the ⋯ appears on row hover or keyboard focus, so a calm
// fleet stays calm. A stuck session also carries its remedy as a plain button up
// beside its status badge (SessionRemedyButton) — this menu is the full set.
//
// Which verbs apply is `lifecycleActions(s)`, shared with the detail header so
// the two surfaces can't drift; the writes are `useSessionActions`. This
// component is only the chrome.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ changed: []; error: [string]; park: ['parked' | 'active'] }>();

const open = ref(false);
const actions = computed(() => lifecycleActions(props.ws));
// Park is the keyboard/no-drag path for the shelf gesture: a live session parks,
// a resting one returns. Archived rows read through their own reveal, not here.
const isShelved = computed(() => shelved(props.ws));
const canPark = computed(() => props.ws.status !== 'archived');
const keepsSession = computed(() => autoArchiveDisabled(props.ws));

function togglePark() {
  open.value = false;
  emit('park', isShelved.value ? 'active' : 'parked');
}

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
</script>

<template>
  <!-- `relative z-10` lifts the control above the row's stretched-link overlay,
       so clicking ⋯ opens the menu instead of opening the session. -->
  <div class="relative z-10 shrink-0">
    <button
      type="button"
      data-testid="row-actions"
      :aria-label="`Actions for ${ws.branch.title || ws.branch.name}`"
      :aria-expanded="open"
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
      data-testid="row-actions-menu"
      class="absolute right-0 top-full z-30 mt-1 w-64 overflow-hidden rounded border border-line bg-surface py-1 shadow-lg"
    >
      <button
        v-if="canPark"
        type="button"
        data-testid="row-action-park"
        class="block w-full border-b border-line px-3 py-1.5 text-left text-fg transition-colors hover:bg-subtle"
        @click="togglePark"
      >
        <span class="block text-xs font-medium">{{ isShelved ? 'Keep live' : 'Park' }}</span>
        <span class="block text-2xs text-faint">{{
          isShelved ? 'Return it to the live list' : 'Rest it on the shelf — kept, not archived'
        }}</span>
      </button>
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
