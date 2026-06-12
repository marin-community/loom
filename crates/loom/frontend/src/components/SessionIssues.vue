<script setup lang="ts">
import type { Issue } from '../types';
import TagPill from './TagPill.vue';

// The Issues tab: the session's own claimed work plus the repo's unclaimed
// backlog. Read-only here — both are managed with `weaver issue` or the
// top-level Issues pane; tags render as quiet, non-deletable pills.
defineProps<{ issues: Issue[]; backlog: Issue[] }>();
</script>

<template>
  <div class="space-y-5">
    <section
      v-if="issues.length"
      class="rounded border border-line bg-surface p-4"
      data-testid="issues-panel"
    >
      <div class="mb-2 flex items-center justify-between">
        <span class="text-xs text-muted">
          Open issues
          <span class="text-faint">({{ issues.length }})</span>
        </span>
        <span class="text-xs text-faint">read-only · manage with <code>weaver issue</code></span>
      </div>
      <ul class="space-y-2 text-sm">
        <li v-for="i in issues" :key="i.id" class="rounded bg-canvas/60 p-2">
          <div class="flex items-baseline gap-2">
            <span class="font-mono text-xs text-muted">#{{ i.id }}</span>
            <span class="text-fg">{{ i.title }}</span>
          </div>
          <pre v-if="i.body" class="mt-1 whitespace-pre-wrap text-xs text-muted">{{ i.body }}</pre>
          <div v-if="i.tags.length" class="mt-1.5 flex flex-wrap items-center gap-1.5">
            <TagPill v-for="t in i.tags" :key="t.key" :tag="t" readonly />
          </div>
        </li>
      </ul>
    </section>

    <section
      v-if="backlog.length"
      class="rounded border border-line bg-surface p-4"
      data-testid="backlog-panel"
    >
      <div class="mb-2 flex items-center justify-between">
        <span class="text-xs text-muted">
          Repo backlog
          <span class="text-faint">({{ backlog.length }})</span>
        </span>
        <span class="text-xs text-faint">unclaimed · whole repo</span>
      </div>
      <ul class="space-y-1 text-sm">
        <li
          v-for="i in backlog"
          :key="i.id"
          class="flex items-baseline gap-2 rounded bg-canvas/60 p-2"
        >
          <span class="font-mono text-xs text-muted">#{{ i.id }}</span>
          <span class="text-fg">{{ i.title }}</span>
        </li>
      </ul>
    </section>

    <p v-if="!issues.length && !backlog.length" class="text-sm text-faint">
      No issues claimed, and the repo backlog is empty.
    </p>
  </div>
</template>
