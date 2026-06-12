<script setup lang="ts">
// Work-area sub-nav. Terminal/Overview are component-local tabs (the terminal
// must never unmount, so the parent flips a ref and v-shows it); Files is a real
// route to FileBrowser (Monaco is heavy and mustn't load on session-open).
// Neutral underline indicator — no loud fills; only the active tab gets text-fg
// + an accent underline.
//
// Terminal is the working zone (live agent + scratch drop); Overview is the
// read-only context (goal, claimed issues, activity) — the issue count rides on
// the Overview tab as a quiet pill rather than owning a tab of its own.
type Tab = 'terminal' | 'overview' | 'files';

defineProps<{ tab: Tab; id: string; issueCount: number }>();
defineEmits<{ select: [Exclude<Tab, 'files'>] }>();

const LOCAL_TABS: { key: Exclude<Tab, 'files'>; label: string }[] = [
  { key: 'terminal', label: 'Terminal' },
  { key: 'overview', label: 'Overview' },
];
</script>

<template>
  <!-- pl-0.5 mirrors the header's 2px left wash border so tab labels align
       with the title above. -->
  <nav class="mb-2 flex items-center gap-1 border-b border-line pl-0.5 text-sm">
    <button
      v-for="t in LOCAL_TABS"
      :key="t.key"
      type="button"
      :data-tab="t.key"
      class="-mb-px border-b-2 px-2.5 py-1.5"
      :class="tab === t.key
        ? 'border-accent text-fg font-medium'
        : 'border-transparent text-muted hover:text-fg'"
      @click="$emit('select', t.key)"
    >
      {{ t.label }}
      <span v-if="t.key === 'overview' && issueCount" class="pill ml-1">{{ issueCount }}</span>
    </button>
    <router-link
      :to="`/s/${id}/files`"
      data-tab="files"
      class="-mb-px border-b-2 px-2.5 py-1.5"
      :class="tab === 'files'
        ? 'border-accent text-fg font-medium'
        : 'border-transparent text-muted hover:text-fg'"
    >
      Files
    </router-link>
  </nav>
</template>
