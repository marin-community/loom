<script setup lang="ts">
import { computed } from 'vue';
import type { GithubStatus } from '../types';
import {
  githubChecksChip,
  githubCompactChip,
  githubConflictChip,
  githubReviewChip,
  githubStateChip,
  type GithubChip,
} from '../lib/githubStatus';
import { compactAge, exactTime } from '../lib/time';

// A branch's GitHub pull-request snapshot, fetched server-side via `gh`. Tints
// are text-color only (never a loud fill) and use GitHub's own familiar hue
// language — green open/passing, violet merged, red failing — so the PR state
// reads at a glance without ever borrowing the reserved loud amber/red
// attention fill. Tokens are semantic (text-ok / text-agent / text-block / …)
// so they swap with the light/dark theme.
const props = defineProps<{ gh: GithubStatus; compact?: boolean }>();

// `draft` reads as its own state while the PR is open; merged/closed win out.
const stateChip = computed(() => githubStateChip(props.gh));
const reviewChip = computed(() => githubReviewChip(props.gh));
const checksChip = computed(() => githubChecksChip(props.gh));
// Only surface mergeability when it's a problem — a clean PR needn't say so.
const conflicting = computed(() => githubConflictChip(props.gh));
const compactChip = computed(() => githubCompactChip(props.gh));
const headAge = computed(() => compactAge(props.gh.head_updated_at));
const headAgeClass = computed(() => {
  if (!props.gh.head_updated_at) return 'text-faint';
  const ageMs = Date.now() - Date.parse(props.gh.head_updated_at);
  if (!Number.isFinite(ageMs) || ageMs >= 24 * 60 * 60 * 1000) return 'text-faint';
  return ageMs < 60 * 60 * 1000 ? 'text-ok' : 'text-muted';
});
const compactDescription = computed(() => {
  const descriptions: Record<string, string> = {
    OK: 'CI passed',
    TESTING: 'CI is running',
    FAILED: 'CI failed',
    PENDING: 'CI has not reported checks',
    DRAFT: 'draft pull request',
    MERGED: 'merged pull request',
    CLOSED: 'closed pull request',
  };
  return descriptions[compactChip.value.key] ?? compactChip.value.label;
});
</script>

<template>
  <!-- Compact: a single tight line for the dashboard's far-right column. -->
  <span
    v-if="compact"
    class="inline-flex items-center gap-1.5 text-xs"
    data-testid="github-compact"
  >
    <a
      :href="gh.pr_url"
      target="_blank"
      rel="noopener"
      class="font-mono text-accent hover:underline"
      @click.stop
      >PR #{{ gh.pr_number }}</a
    >
    <span
      :class="compactChip.cls"
      class="font-mono uppercase tracking-wide"
      :title="compactDescription"
    >
      {{ compactChip.label }}
    </span>
    <time
      v-if="gh.head_updated_at"
      :datetime="gh.head_updated_at"
      :title="`PR head updated ${exactTime(gh.head_updated_at)}`"
      :aria-label="`PR head updated ${exactTime(gh.head_updated_at)}`"
      :class="headAgeClass"
      class="font-mono"
      data-testid="github-head-age"
    >
      {{ headAge }}
    </time>
  </span>

  <!-- Full: a labelled block for the session overview. -->
  <div v-else class="space-y-2" data-testid="github-full">
    <a
      :href="gh.pr_url"
      target="_blank"
      rel="noopener"
      class="block text-sm text-accent hover:underline"
    >
      <span class="font-mono">#{{ gh.pr_number }}</span>
      <span class="ml-1 text-fg">{{ gh.pr_title }}</span>
    </a>
    <div class="flex flex-wrap items-center gap-2">
      <span
        v-for="chip in [stateChip, reviewChip, checksChip].filter(Boolean)"
        :key="(chip as GithubChip).label"
        :class="(chip as GithubChip).cls"
        class="rounded bg-subtle px-1.5 py-0.5 text-[0.7rem] font-medium font-mono uppercase tracking-wide"
      >
        {{ (chip as GithubChip).label }}
      </span>
      <time
        v-if="gh.head_updated_at"
        :datetime="gh.head_updated_at"
        :title="exactTime(gh.head_updated_at)"
        class="rounded bg-subtle px-1.5 py-0.5 font-mono text-[0.7rem] text-muted"
      >
        updated {{ headAge }}
      </time>
      <span
        v-if="conflicting"
        :class="conflicting.cls"
        class="rounded bg-subtle px-1.5 py-0.5 text-[0.7rem] font-medium font-mono uppercase tracking-wide"
      >
        {{ conflicting.label }}
      </span>
    </div>
  </div>
</template>
