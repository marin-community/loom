<script setup lang="ts">
import { computed, ref } from 'vue';
import type { Session } from '../types';
import { clearSessionGithub, patchIssue, refreshSessionGithub, setSessionGithub } from '../api';

// The two GitHub links a workstream accumulates: the issue it came from and the
// PR it produces. Both pills remain visible when empty so association is a
// discoverable operation rather than a setting hidden in the manage menu.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ reload: [] }>();

const prOpen = ref(false);
const prDraft = ref('');
const prBusy = ref('');
const prError = ref('');
const prNumber = computed(() => props.ws.branch.github?.pr_number ?? props.ws.branch.github_pr);
const prStateClass = computed(() => {
  const state = props.ws.branch.github?.pr_state;
  return (
    { OPEN: 'text-ok', MERGED: 'text-agent', CLOSED: 'text-block' }[state ?? ''] ?? 'text-muted'
  );
});
const prChecksClass = computed(() => {
  const checks = props.ws.branch.github?.checks;
  return (
    { passing: 'text-ok', failing: 'text-block', pending: 'text-info' }[checks ?? ''] ??
    'text-muted'
  );
});

const issueOpen = ref(false);
const issueDraft = ref('');
const issueBusy = ref(false);
const issueError = ref('');

function togglePrEditor() {
  prOpen.value = !prOpen.value;
  issueOpen.value = false;
  prError.value = '';
  prDraft.value = String(props.ws.branch.github_pr ?? props.ws.branch.github?.pr_number ?? '');
}

async function updatePr(action: 'set' | 'auto' | 'refresh') {
  if (prBusy.value) return;
  const number = Number(prDraft.value);
  if (action === 'set' && (!Number.isInteger(number) || number <= 0)) {
    prError.value = 'Enter a positive PR number.';
    return;
  }
  prBusy.value = action;
  prError.value = '';
  try {
    if (action === 'set') await setSessionGithub(props.ws.id, number);
    else if (action === 'auto') await clearSessionGithub(props.ws.id);
    else await refreshSessionGithub(props.ws.id);
    prOpen.value = false;
    emit('reload');
  } catch (e) {
    prError.value = (e as Error).message;
  } finally {
    prBusy.value = '';
  }
}

function toggleIssueEditor() {
  issueOpen.value = !issueOpen.value;
  prOpen.value = false;
  issueError.value = '';
  issueDraft.value = props.ws.github_issue
    ? `${props.ws.github_issue.repo}#${props.ws.github_issue.number}`
    : '';
}

async function updateIssue(clear = false) {
  if (issueBusy.value) return;
  if (!props.ws.tracking_issue) {
    issueError.value = 'This session has no tracking issue to associate.';
    return;
  }
  issueBusy.value = true;
  issueError.value = '';
  try {
    await patchIssue(props.ws.tracking_issue, { github: clear ? '' : issueDraft.value.trim() });
    issueOpen.value = false;
    emit('reload');
  } catch (e) {
    issueError.value = (e as Error).message;
  } finally {
    issueBusy.value = false;
  }
}
</script>

<template>
  <div class="relative shrink-0">
    <button
      type="button"
      class="pill font-mono hover:border-accent hover:text-accent"
      data-testid="pr-association-pill"
      :aria-expanded="prOpen"
      :title="prNumber ? 'Edit pull request association' : 'Associate a pull request'"
      @click="togglePrEditor"
    >
      PR {{ prNumber ? `#${prNumber}` : '—' }}
    </button>
    <div
      v-if="prOpen"
      class="absolute left-0 top-full z-30 mt-1 w-64 rounded border border-line bg-surface p-3 shadow-lg"
      data-testid="pr-mapping-popover"
    >
      <form class="space-y-2" data-testid="pr-mapping-form" @submit.prevent="updatePr('set')">
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-2xs font-semibold uppercase tracking-wider text-muted"
            >Pull request</span
          >
          <a
            v-if="ws.branch.github"
            :href="ws.branch.github.pr_url"
            target="_blank"
            rel="noopener"
            class="text-2xs text-accent hover:underline"
            >Open on GitHub ↗</a
          >
        </div>
        <label class="block text-2xs text-muted">
          PR number
          <input
            v-model="prDraft"
            type="number"
            min="1"
            class="mt-1 block w-full rounded bg-input px-2 py-1.5 font-mono text-xs text-fg"
          />
        </label>
        <p class="text-2xs text-faint">
          {{ ws.branch.github_pr ? 'Pinned manually.' : 'Following the worktree branch.' }}
        </p>
        <p v-if="prError" class="text-xs text-block">{{ prError }}</p>
        <div class="flex flex-wrap gap-1.5">
          <button type="submit" class="btn-primary px-2 py-1 text-xs" :disabled="!!prBusy">
            {{ prBusy === 'set' ? 'Saving…' : 'Pin PR' }}
          </button>
          <button
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            :disabled="!!prBusy"
            @click="updatePr('auto')"
          >
            Use current
          </button>
          <button
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            :disabled="!!prBusy"
            @click="updatePr('refresh')"
          >
            Refresh
          </button>
        </div>
      </form>
    </div>
  </div>
  <template v-if="ws.branch.github">
    <span :class="prStateClass" class="font-mono uppercase tracking-wide">
      {{ ws.branch.github.pr_state.toLowerCase() }}
    </span>
    <span
      v-if="ws.branch.github.checks"
      :class="prChecksClass"
      class="font-mono"
      :title="`Checks ${ws.branch.github.checks}`"
      >●</span
    >
  </template>

  <div class="relative shrink-0">
    <button
      type="button"
      class="pill font-mono hover:border-accent hover:text-accent disabled:cursor-not-allowed disabled:opacity-60"
      data-testid="issue-association-pill"
      :aria-expanded="issueOpen"
      :disabled="!ws.tracking_issue"
      :title="
        ws.tracking_issue
          ? ws.github_issue
            ? 'Edit GitHub issue association'
            : 'Associate a GitHub issue'
          : 'This session has no tracking issue'
      "
      @click="toggleIssueEditor"
    >
      Issue {{ ws.github_issue ? `#${ws.github_issue.number}` : '—' }}
    </button>
    <div
      v-if="issueOpen"
      class="absolute left-0 top-full z-30 mt-1 w-72 rounded border border-line bg-surface p-3 shadow-lg"
      data-testid="issue-mapping-popover"
    >
      <form class="space-y-2" data-testid="issue-mapping-form" @submit.prevent="updateIssue()">
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-2xs font-semibold uppercase tracking-wider text-muted"
            >GitHub issue</span
          >
          <a
            v-if="ws.github_issue"
            :href="`https://github.com/${ws.github_issue.repo}/issues/${ws.github_issue.number}`"
            target="_blank"
            rel="noopener"
            class="text-2xs text-accent hover:underline"
            >Open on GitHub ↗</a
          >
        </div>
        <label class="block text-2xs text-muted">
          owner/repo#number
          <input
            v-model="issueDraft"
            placeholder="acme/widgets#123"
            class="mt-1 block w-full rounded bg-input px-2 py-1.5 font-mono text-xs text-fg"
          />
        </label>
        <p v-if="issueError" class="text-xs text-block">{{ issueError }}</p>
        <div class="flex gap-1.5">
          <button type="submit" class="btn-primary px-2 py-1 text-xs" :disabled="issueBusy">
            {{ issueBusy ? 'Saving…' : 'Save' }}
          </button>
          <button
            v-if="ws.github_issue"
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            :disabled="issueBusy"
            @click="updateIssue(true)"
          >
            Clear
          </button>
        </div>
      </form>
    </div>
  </div>
</template>
