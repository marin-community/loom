<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import type { Session } from '../types';
import { clearSessionGithub, patchIssue, refreshSessionGithub, setSessionGithub } from '../api';

// The two high-frequency GitHub destinations a workstream accumulates: the issue
// it came from and the PR it produces. A mapped pill is a direct link; its
// adjacent edit control owns reassociation. An empty pill opens that same editor,
// keeping setup discoverable without making every follow a two-click action.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ reload: [] }>();

type Editor = 'pr' | 'issue';
const root = ref<HTMLElement | null>(null);
const prTrigger = ref<HTMLButtonElement | null>(null);
const issueTrigger = ref<HTMLButtonElement | null>(null);
const editor = ref<Editor | null>(null);
const prOpen = computed(() => editor.value === 'pr');
const issueOpen = computed(() => editor.value === 'issue');

const prDraft = ref('');
const prBusy = ref('');
const prError = ref('');
const prNumber = computed(() => props.ws.branch.github?.pr_number ?? props.ws.branch.github_pr);
const prUrl = computed(() => {
  if (props.ws.branch.github?.pr_url) return props.ws.branch.github.pr_url;
  if (!props.ws.github_repo || !prNumber.value) return '';
  return `https://github.com/${props.ws.github_repo}/pull/${prNumber.value}`;
});
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

const issueDraft = ref('');
const issueBusy = ref(false);
const issueError = ref('');
const issueUrl = computed(() => {
  const issue = props.ws.github_issue;
  return issue ? `https://github.com/${issue.repo}/issues/${issue.number}` : '';
});

function triggerFor(target: Editor): HTMLButtonElement | null {
  return target === 'pr' ? prTrigger.value : issueTrigger.value;
}

function closeEditor(focusTrigger = false) {
  const active = editor.value;
  if (!active) return;
  editor.value = null;
  if (focusTrigger) void nextTick(() => triggerFor(active)?.focus());
}

function togglePrEditor() {
  if (prOpen.value) {
    closeEditor(true);
    return;
  }
  editor.value = 'pr';
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
    closeEditor(true);
    emit('reload');
  } catch (e) {
    prError.value = (e as Error).message;
  } finally {
    prBusy.value = '';
  }
}

function toggleIssueEditor() {
  if (issueOpen.value) {
    closeEditor(true);
    return;
  }
  editor.value = 'issue';
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
    closeEditor(true);
    emit('reload');
  } catch (e) {
    issueError.value = (e as Error).message;
  } finally {
    issueBusy.value = false;
  }
}

// Match the header's Details popover: outside pointer presses light-dismiss
// without swallowing the target, while Escape closes and restores the trigger.
function onOutsidePointerDown(event: PointerEvent) {
  const target = event.target;
  if (!(target instanceof Element) || root.value?.contains(target)) return;
  closeEditor();
}

watch(editor, async (open) => {
  document.removeEventListener('pointerdown', onOutsidePointerDown, true);
  if (!open) return;
  await nextTick();
  if (editor.value) document.addEventListener('pointerdown', onOutsidePointerDown, true);
});
onBeforeUnmount(() => document.removeEventListener('pointerdown', onOutsidePointerDown, true));
</script>

<template>
  <div
    ref="root"
    class="flex shrink-0 flex-wrap items-center justify-end gap-1.5 text-xs"
    data-testid="github-associations"
    @keydown.esc.stop.prevent="closeEditor(true)"
  >
    <div class="relative flex shrink-0 items-center gap-0.5">
      <a
        v-if="prUrl"
        :href="prUrl"
        target="_blank"
        rel="noopener"
        class="pill font-mono hover:border-accent hover:text-accent"
        data-testid="pr-association-pill"
        :title="`Open pull request #${prNumber} on GitHub`"
      >
        PR #{{ prNumber }}
      </a>
      <button
        v-else
        ref="prTrigger"
        type="button"
        class="pill font-mono hover:border-accent hover:text-accent"
        data-testid="pr-association-pill"
        :aria-expanded="prOpen"
        :title="prNumber ? 'Edit pull request association' : 'Associate a pull request'"
        @click="togglePrEditor"
      >
        PR {{ prNumber ? `#${prNumber}` : '—' }}
      </button>
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
      <button
        v-if="prUrl"
        ref="prTrigger"
        type="button"
        class="min-h-7 rounded px-1 text-faint hover:bg-subtle hover:text-fg"
        data-testid="pr-association-edit"
        :aria-expanded="prOpen"
        title="Edit pull request association"
        aria-label="Edit pull request association"
        @click="togglePrEditor"
      >
        ✎
      </button>
      <div
        v-if="prOpen"
        class="absolute right-0 top-full z-30 mt-1 w-64 rounded border border-line bg-surface p-3 shadow-lg"
        data-testid="pr-mapping-popover"
      >
        <form class="space-y-2" data-testid="pr-mapping-form" @submit.prevent="updatePr('set')">
          <div class="flex items-baseline justify-between gap-2">
            <span class="text-2xs font-semibold uppercase tracking-wider text-muted">
              Pull request
            </span>
            <a
              v-if="prUrl"
              :href="prUrl"
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
              autocomplete="off"
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

    <div class="relative flex shrink-0 items-center gap-0.5">
      <a
        v-if="issueUrl"
        :href="issueUrl"
        target="_blank"
        rel="noopener"
        class="pill font-mono hover:border-accent hover:text-accent"
        data-testid="issue-association-pill"
        :title="`Open ${ws.github_issue?.repo} issue #${ws.github_issue?.number} on GitHub`"
      >
        Issue #{{ ws.github_issue?.number }}
      </a>
      <button
        v-else
        ref="issueTrigger"
        type="button"
        class="pill font-mono hover:border-accent hover:text-accent disabled:cursor-not-allowed disabled:opacity-60"
        data-testid="issue-association-pill"
        :aria-expanded="issueOpen"
        :disabled="!ws.tracking_issue"
        :title="
          ws.tracking_issue ? 'Associate a GitHub issue' : 'This session has no tracking issue'
        "
        @click="toggleIssueEditor"
      >
        Issue —
      </button>
      <button
        v-if="issueUrl"
        ref="issueTrigger"
        type="button"
        class="min-h-7 rounded px-1 text-faint hover:bg-subtle hover:text-fg"
        data-testid="issue-association-edit"
        :aria-expanded="issueOpen"
        title="Edit GitHub issue association"
        aria-label="Edit GitHub issue association"
        @click="toggleIssueEditor"
      >
        ✎
      </button>
      <div
        v-if="issueOpen"
        class="absolute right-0 top-full z-30 mt-1 w-72 rounded border border-line bg-surface p-3 shadow-lg"
        data-testid="issue-mapping-popover"
      >
        <form class="space-y-2" data-testid="issue-mapping-form" @submit.prevent="updateIssue()">
          <div class="flex items-baseline justify-between gap-2">
            <span class="text-2xs font-semibold uppercase tracking-wider text-muted">
              GitHub issue
            </span>
            <a
              v-if="issueUrl"
              :href="issueUrl"
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
              autocomplete="off"
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
  </div>
</template>
