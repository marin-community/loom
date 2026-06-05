<script setup lang="ts">
// Work-area sub-nav. Terminal/Overview/Issues are component-local tabs (the
// terminal must never unmount, so the parent flips a ref and v-shows it);
// Files is a real route to FileBrowser (Monaco is heavy and mustn't load on
// session-open). Neutral underline indicator — no loud fills; only the active
// tab gets text-fg + an accent underline.
type Tab = 'terminal' | 'overview' | 'issues' | 'files';

const props = defineProps<{ tab: Tab; id: string; issueCount: number }>();
defineEmits<{ select: [Exclude<Tab, 'files'>] }>();

const LOCAL_TABS: { key: Exclude<Tab, 'files'>; label: string }[] = [
  { key: 'terminal', label: 'Terminal' },
  { key: 'overview', label: 'Overview' },
  { key: 'issues', label: 'Issues' },
];
</script>

<template>
  <nav class="mb-3 flex items-center gap-1 border-b border-line text-sm">
    <button
      v-for="t in LOCAL_TABS"
      :key="t.key"
      type="button"
      :data-tab="t.key"
      class="-mb-px border-b-2 px-3 py-2"
      :class="tab === t.key
        ? 'border-accent text-fg font-medium'
        : 'border-transparent text-muted hover:text-fg'"
      @click="$emit('select', t.key)"
    >
      {{ t.label }}
      <span v-if="t.key === 'issues' && issueCount" class="pill ml-1">{{ issueCount }}</span>
    </button>
    <router-link
      :to="`/s/${id}/files`"
      data-tab="files"
      class="-mb-px border-b-2 px-3 py-2"
      :class="tab === 'files'
        ? 'border-accent text-fg font-medium'
        : 'border-transparent text-muted hover:text-fg'"
    >
      Files
    </router-link>
  </nav>
</template>
