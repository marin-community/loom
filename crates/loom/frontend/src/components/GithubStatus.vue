<script setup lang="ts">
import { computed } from 'vue';
import type { GithubStatus } from '../types';

// A branch's GitHub pull-request snapshot, fetched server-side via `gh`. Tints
// are text-color only (never a loud fill) and use GitHub's own familiar hue
// language — green open/passing, violet merged, red failing — so the PR state
// reads at a glance without ever borrowing the reserved loud amber/red
// attention fill. Tokens are semantic (text-ok / text-agent / text-block / …)
// so they swap with the light/dark theme.
const props = defineProps<{ gh: GithubStatus; compact?: boolean }>();

interface Chip {
  label: string;
  cls: string;
}

// `draft` reads as its own state while the PR is open; merged/closed win out.
const stateChip = computed<Chip>(() => {
  const draft = props.gh.is_draft && props.gh.pr_state === 'OPEN';
  const key = draft ? 'DRAFT' : props.gh.pr_state;
  const tint: Record<string, string> = {
    OPEN: 'text-ok',
    MERGED: 'text-agent',
    CLOSED: 'text-block',
    DRAFT: 'text-faint',
  };
  return { label: key.toLowerCase(), cls: tint[key] ?? 'text-muted' };
});

const reviewChip = computed<Chip | null>(() => {
  const r = props.gh.review_decision;
  if (!r) return null;
  const map: Record<string, Chip> = {
    APPROVED: { label: 'approved', cls: 'text-ok' },
    CHANGES_REQUESTED: { label: 'changes requested', cls: 'text-block' },
    REVIEW_REQUIRED: { label: 'review required', cls: 'text-muted' },
  };
  return map[r] ?? { label: r.toLowerCase().replace(/_/g, ' '), cls: 'text-muted' };
});

const checksChip = computed<Chip | null>(() => {
  const c = props.gh.checks;
  if (!c) return null;
  const map: Record<string, Chip> = {
    passing: { label: 'checks passing', cls: 'text-ok' },
    failing: { label: 'checks failing', cls: 'text-block' },
    pending: { label: 'checks pending', cls: 'text-info' },
  };
  return map[c] ?? { label: `checks ${c}`, cls: 'text-muted' };
});

// Only surface mergeability when it's a problem — a clean PR needn't say so.
const conflicting = computed(() => props.gh.mergeable === 'CONFLICTING');
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
    <span :class="stateChip.cls" class="font-mono uppercase tracking-wide">{{
      stateChip.label
    }}</span>
    <span v-if="checksChip" :class="checksChip.cls" class="font-mono" title="CI checks">●</span>
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
      <span class="text-fg">{{ gh.pr_title }}</span>
    </a>
    <div class="flex flex-wrap items-center gap-2">
      <span
        v-for="chip in [stateChip, reviewChip, checksChip].filter(Boolean)"
        :key="(chip as Chip).label"
        :class="(chip as Chip).cls"
        class="rounded bg-subtle px-1.5 py-0.5 text-[0.7rem] font-medium font-mono uppercase tracking-wide"
      >
        {{ (chip as Chip).label }}
      </span>
      <span
        v-if="conflicting"
        class="rounded bg-subtle px-1.5 py-0.5 text-[0.7rem] font-medium font-mono uppercase tracking-wide text-block"
      >
        conflicts
      </span>
    </div>
  </div>
</template>
