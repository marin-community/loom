<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref } from 'vue';
import {
  addReviewComment,
  createReview,
  deleteReviewComment,
  discardReview,
  getChanges,
  listChangesReviews,
  retryReviewDelivery,
  retargetReviewToCurrent,
  submitReview,
  updateReview,
  updateReviewComment,
  ApiError,
} from '../api';
import type { ChangeAnchor, ChangeFile, ChangeHunk, ChangeLine, ChangeSet, Review } from '../types';
import { ReviewDraftController } from '../lib/reviewDraftController';
import ReviewCommentCard from './ReviewCommentCard.vue';
import ReviewTray from './ReviewTray.vue';

const props = defineProps<{ id: string }>();
const changes = ref<ChangeSet | null>(null);
const reviews = ref<Review[]>([]);
const loading = ref(false);
const error = ref('');
const notice = ref('');
const expanded = reactive(new Set<string>());
const activeComment = ref<number | null>(null);
const reanchorComment = ref<number | null>(null);
const commentErrors = reactive<Record<number, string>>({});
const deliveryErrors = reactive<Record<number, string>>({});
const trayOpen = ref(false);
const trayError = ref('');
const overallNote = ref('');
const summaryDirty = ref(false);
const summarySaving = ref(false);
const acknowledgeOutdated = ref(false);
const submitting = ref(false);
const discarding = ref(false);

type Pending = { anchor: ChangeAnchor; body: string; version: string };
const pending = ref<Pending | null>(null);
const savingComment = ref(false);
const composerInput = ref<HTMLTextAreaElement | null>(null);

const draft = computed(() => reviews.value.find((review) => review.status === 'draft') ?? null);

function replaceReview(next: Review) {
  const index = reviews.value.findIndex((review) => review.id === next.id);
  if (index < 0) reviews.value = [next, ...reviews.value];
  else reviews.value.splice(index, 1, next);
}

function conflictReview(cause: unknown): Review | null {
  if (!(cause instanceof ApiError) || cause.status !== 409) return null;
  const details = cause.body.details;
  if (!details || typeof details !== 'object') return null;
  const fresh = (details as { review?: unknown }).review;
  if (!fresh || typeof fresh !== 'object' || typeof (fresh as Review).id !== 'number') return null;
  const review = fresh as Review;
  if (review.status === 'draft') controller.reconcile(review);
  else {
    const dirtySummary = controller.summaryDirty ? overallNote.value : null;
    replaceReview(review);
    controller.clearOwnership();
    if (dirtySummary != null) controller.editSummary(dirtySummary);
    activeComment.value = null;
    reanchorComment.value = null;
  }
  return review;
}

function mutationMessage(cause: unknown): string {
  if (conflictReview(cause)) {
    return 'This draft changed elsewhere. The latest version is loaded; review it before retrying.';
  }
  return (cause as Error).message;
}

const controller = new ReviewDraftController<Review>({
  saveSummary: async (current, summary) => {
    const version = changes.value?.version;
    if (!version) throw new Error('The change-set version is unavailable.');
    const review =
      current ??
      (await createReview(props.id, {
        subject_kind: 'changes',
        subject_key: 'changes',
        subject_version: version,
      }));
    return updateReview(review.id, {
      expected_revision: review.draft_revision,
      summary,
    });
  },
  onDraft: (next) => {
    if (next) replaceReview(next);
  },
  onSummary: (summary, dirty) => {
    overallNote.value = summary;
    summaryDirty.value = dirty;
  },
});

async function load() {
  const epoch = controller.beginRefresh();
  loading.value = true;
  try {
    const [nextChanges, nextReviews] = await Promise.all([
      getChanges(props.id),
      listChangesReviews(props.id),
    ]);
    const nextDraft = nextReviews.find((review) => review.status === 'draft') ?? null;
    if (!controller.acceptRefresh(epoch, nextDraft)) return;
    changes.value = nextChanges;
    reviews.value = nextReviews;
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loading.value = false;
  }
}

function fileKey(file: ChangeFile): string {
  return file.path.bytes;
}

function toggleFile(file: ChangeFile) {
  const key = fileKey(file);
  if (expanded.has(key)) expanded.delete(key);
  else expanded.add(key);
}

function lineSide(line: ChangeLine): 'old' | 'new' {
  return line.kind === 'deletion' ? 'old' : 'new';
}

function lineNumber(line: ChangeLine, side: 'old' | 'new'): number | null {
  return side === 'old' ? line.old_line : line.new_line;
}

function anchorFor(
  file: ChangeFile,
  hunk: ChangeHunk,
  line: ChangeLine,
  last = line,
): ChangeAnchor | null {
  const side = lineSide(line);
  const start = lineNumber(line, side);
  const end = lineNumber(last, side);
  if (start == null || end == null) return null;
  const low = Math.min(start, end);
  const high = Math.max(start, end);
  const eligible = hunk.lines.filter((line) => lineNumber(line, side) != null);
  const selected = eligible.filter((line) => {
    const number = lineNumber(line, side)!;
    return number >= low && number <= high;
  });
  if (selected.length !== high - low + 1) return null;
  const first = eligible.indexOf(selected[0]);
  const lastIndex = eligible.indexOf(selected.at(-1)!);
  return {
    path: file.path,
    side,
    start_line: low,
    end_line: high,
    hunk_header: hunk.header,
    context_before: eligible.slice(Math.max(0, first - 2), first).map((line) => line.text),
    selected: selected.map((line) => line.text),
    context_after: eligible.slice(lastIndex + 1, lastIndex + 3).map((line) => line.text),
  };
}

function selectLine(file: ChangeFile, hunk: ChangeHunk, line: ChangeLine) {
  const version = changes.value?.version;
  if (!version) return;
  const next = anchorFor(file, hunk, line);
  if (!next) return;
  if (reanchorComment.value != null) {
    void applyReanchor(reanchorComment.value, next);
    return;
  }
  const current = pending.value?.anchor;
  if (
    pending.value?.version === version &&
    current?.path.bytes === file.path.bytes &&
    current.side === next.side &&
    current.hunk_header === hunk.header
  ) {
    const first = hunk.lines.find(
      (candidate) => lineNumber(candidate, current.side) === current.start_line,
    );
    const range = first && anchorFor(file, hunk, first, line);
    if (range) {
      pending.value = { ...pending.value!, anchor: range };
      return;
    }
  }
  pending.value = { anchor: next, body: pending.value?.body ?? '', version };
  void nextTick(() => composerInput.value?.focus());
}

function extendSelection(file: ChangeFile, hunk: ChangeHunk, line: ChangeLine, direction: -1 | 1) {
  const version = changes.value?.version;
  if (!version) return;
  const side = lineSide(line);
  const eligible = hunk.lines.filter((candidate) => lineNumber(candidate, side) != null);
  const current = pending.value;
  const sameRange =
    current?.version === version &&
    current.anchor.path.bytes === file.path.bytes &&
    current.anchor.side === side &&
    current.anchor.hunk_header === hunk.header;
  const boundary = sameRange
    ? direction < 0
      ? current!.anchor.start_line
      : current!.anchor.end_line
    : lineNumber(line, side);
  const boundaryIndex = eligible.findIndex((candidate) => lineNumber(candidate, side) === boundary);
  const target = eligible[boundaryIndex + direction];
  if (boundaryIndex < 0 || !target) return;
  let range: ChangeAnchor | null;
  if (!sameRange) {
    range =
      direction < 0 ? anchorFor(file, hunk, target, line) : anchorFor(file, hunk, line, target);
  } else {
    const first = hunk.lines.find(
      (candidate) => lineNumber(candidate, side) === current!.anchor.start_line,
    );
    const last =
      direction < 0
        ? hunk.lines.find((candidate) => lineNumber(candidate, side) === current!.anchor.end_line)
        : target;
    range =
      first && last && direction < 0
        ? anchorFor(file, hunk, target, last)
        : first && last
          ? anchorFor(file, hunk, first, target)
          : null;
  }
  if (!range) return;
  pending.value = { anchor: range, body: current?.body ?? '', version };
}

async function saveComment() {
  const capture = pending.value;
  const body = capture?.body.trim();
  if (!capture || !body || savingComment.value) return;
  savingComment.value = true;
  trayError.value = '';
  try {
    const updated = await controller.command(async (current) => {
      const review =
        current ??
        (await createReview(props.id, {
          subject_kind: 'changes',
          subject_key: 'changes',
          subject_version: capture.version,
        }));
      return addReviewComment(review.id, {
        expected_revision: review.draft_revision,
        subject_version: capture.version,
        anchor_kind: 'change',
        anchor: capture.anchor,
        body,
      });
    });
    activeComment.value = updated.comments.at(-1)?.id ?? null;
    pending.value = null;
    trayOpen.value = true;
    notice.value = 'Pending comment saved.';
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    savingComment.value = false;
  }
}

async function editComment(payload: { commentId: number; body: string }) {
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return updateReviewComment(current.id, payload.commentId, {
        expected_revision: current.draft_revision,
        body: payload.body,
      });
    });
  } catch (cause) {
    commentErrors[payload.commentId] = mutationMessage(cause);
  }
}

async function removeComment(commentId: number) {
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return deleteReviewComment(current.id, commentId, current.draft_revision);
    });
    activeComment.value = null;
  } catch (cause) {
    throw new Error(mutationMessage(cause));
  }
}

async function applyReanchor(commentId: number, anchor: ChangeAnchor) {
  const version = changes.value?.version;
  if (!version) return;
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return updateReviewComment(current.id, commentId, {
        expected_revision: current.draft_revision,
        subject_version: version,
        anchor_kind: 'change',
        anchor,
      });
    });
    reanchorComment.value = null;
    notice.value = 'Comment re-anchored to the current changes.';
  } catch (cause) {
    commentErrors[commentId] = mutationMessage(cause);
  }
}

function editOverall(summary: string) {
  controller.editSummary(summary);
}

async function saveOverall() {
  summarySaving.value = true;
  try {
    await controller.flush();
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    summarySaving.value = false;
  }
}

async function discardDraft() {
  discarding.value = true;
  try {
    const id = await controller.freeze(async (current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      await discardReview(current.id, current.draft_revision);
      return { draft: null, result: current.id };
    });
    reviews.value = reviews.value.filter((review) => review.id !== id);
    trayOpen.value = false;
  } finally {
    discarding.value = false;
  }
}

async function retarget() {
  if (!draft.value || draft.value.comments.length) return;
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return retargetReviewToCurrent(current.id, current.draft_revision);
    });
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  }
}

async function submit() {
  submitting.value = true;
  try {
    const submitted = await controller.freeze(async (current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      const result = await submitReview(current.id, {
        expected_revision: current.draft_revision,
        acknowledge_outdated: acknowledgeOutdated.value,
      });
      replaceReview(result);
      return { draft: null, result };
    });
    notice.value = `Review submitted · ${submitted.delivery_state}.`;
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    submitting.value = false;
  }
}

async function retryDelivery(item: Review) {
  try {
    replaceReview(await retryReviewDelivery(item.id));
  } catch (cause) {
    deliveryErrors[item.id] = (cause as Error).message;
  }
}

function navigate(direction: number) {
  const comments = draft.value?.comments ?? [];
  if (!comments.length) return;
  const current = comments.findIndex((comment) => comment.id === activeComment.value);
  activeComment.value = comments[(current + direction + comments.length) % comments.length].id;
}

onMounted(load);
</script>

<template>
  <section
    class="relative flex h-full min-h-0 flex-col overflow-hidden"
    data-testid="changes-panel"
  >
    <header class="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
      <div class="min-w-0 flex-1">
        <h2 class="text-sm font-semibold text-fg">Changes</h2>
        <p
          v-if="changes?.base.state === 'available'"
          class="truncate font-mono text-2xs text-faint"
        >
          {{ changes.base.reference }} · {{ changes.base.oid.slice(0, 10) }}
        </p>
      </div>
      <span v-if="changes" class="text-xs text-muted">
        {{ changes.totals.files }} files · +{{ changes.totals.additions }} −{{
          changes.totals.deletions
        }}
      </span>
      <button type="button" class="btn-secondary px-2 py-1 text-xs" @click="load">Refresh</button>
    </header>

    <p v-if="error" class="m-3 rounded bg-block-soft p-2 text-xs text-block" role="alert">
      {{ error }}
    </p>
    <p
      v-else-if="changes?.base.state === 'unavailable'"
      class="m-3 rounded border border-line p-3 text-sm text-muted"
    >
      Changes unavailable: {{ changes.base.reason.replaceAll('_', ' ') }} for local
      <code>{{ changes.base.reference }}</code
      >.
    </p>
    <div v-else class="min-h-0 flex-1 overflow-auto">
      <p v-if="loading && !changes" class="p-3 text-sm text-muted">Loading changes…</p>
      <p v-else-if="changes && !changes.files.length" class="p-3 text-sm text-muted">
        No branch or worktree changes.
      </p>
      <p v-if="changes?.truncated" class="m-3 rounded bg-block-soft p-2 text-xs text-block">
        This response reached its explicit display bounds. Refresh after narrowing the change set;
        the version still covers all final bytes when available.
      </p>

      <article v-for="file in changes?.files" :key="file.path.bytes" class="border-b border-line">
        <button
          type="button"
          class="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-subtle"
          :aria-expanded="expanded.has(fileKey(file))"
          @click="toggleFile(file)"
        >
          <span class="w-4 text-faint">{{ expanded.has(fileKey(file)) ? '▾' : '▸' }}</span>
          <span class="rounded bg-subtle px-1.5 py-0.5 text-2xs uppercase text-muted">
            {{ file.status }}
          </span>
          <code class="min-w-0 flex-1 truncate text-xs">{{ file.path.display }}</code>
          <span class="text-2xs text-faint">{{ file.sources.join(' · ') }}</span>
          <span class="font-mono text-2xs text-muted"
            >+{{ file.additions ?? '–' }} −{{ file.deletions ?? '–' }}</span
          >
        </button>

        <div v-if="expanded.has(fileKey(file))" class="overflow-x-auto bg-code font-mono text-xs">
          <p v-if="file.content !== 'text'" class="px-4 py-3 text-muted">
            {{ file.content }} content is not rendered.
          </p>
          <div v-for="hunk in file.hunks" :key="hunk.header">
            <p class="sticky left-0 bg-subtle px-3 py-1 text-accent">{{ hunk.header }}</p>
            <button
              v-for="line in hunk.lines"
              :key="`${line.old_line}:${line.new_line}:${line.kind}`"
              type="button"
              class="grid min-w-full grid-cols-[3rem_3rem_1rem_1fr] text-left hover:bg-subtle focus:bg-subtle"
              :class="{
                'bg-green-950/20': line.kind === 'addition',
                'bg-red-950/20': line.kind === 'deletion',
              }"
              :aria-label="`Comment on ${file.path.display} ${lineSide(line)} line ${lineNumber(line, lineSide(line))}`"
              @click="selectLine(file, hunk, line)"
              @keydown.shift.up.prevent="extendSelection(file, hunk, line, -1)"
              @keydown.shift.down.prevent="extendSelection(file, hunk, line, 1)"
            >
              <span class="select-none px-1 text-right text-faint">{{ line.old_line }}</span>
              <span class="select-none px-1 text-right text-faint">{{ line.new_line }}</span>
              <span class="select-none text-center">{{
                line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '−' : ' '
              }}</span>
              <span class="whitespace-pre px-1">{{ line.text || ' ' }}</span>
            </button>
          </div>
        </div>
      </article>

      <div v-if="draft?.comments.length" class="space-y-1 p-3">
        <template v-if="draft">
          <ReviewCommentCard
            v-for="comment in draft.comments"
            :key="comment.id"
            :review="draft"
            :comment="comment"
            :active="activeComment === comment.id"
            :reanchoring="reanchorComment === comment.id"
            :error="commentErrors[comment.id] ?? ''"
            :delete-action="removeComment"
            @focus="activeComment = $event"
            @close="activeComment = null"
            @edit="editComment"
            @reanchor="reanchorComment = $event"
            @cancel-reanchor="reanchorComment = null"
          />
        </template>
      </div>
    </div>

    <form
      v-if="pending"
      class="absolute bottom-14 left-3 z-30 w-[min(30rem,calc(100%-1.5rem))] rounded border border-accent bg-surface p-2 shadow-xl"
      data-testid="change-comment-composer"
      @submit.prevent="saveComment"
    >
      <p class="mb-1 text-2xs font-semibold uppercase text-accent">
        {{ pending.anchor.path.display }} · {{ pending.anchor.side }}
        {{ pending.anchor.start_line }}–{{ pending.anchor.end_line }}
      </p>
      <textarea
        ref="composerInput"
        v-model="pending.body"
        rows="3"
        class="w-full rounded border border-line bg-input p-2 text-xs"
      ></textarea>
      <div class="mt-2 flex justify-end gap-2">
        <button type="button" class="btn-secondary px-2 py-1 text-xs" @click="pending = null">
          Cancel
        </button>
        <button
          type="submit"
          class="btn-primary px-2 py-1 text-xs"
          :disabled="!pending.body.trim() || savingComment"
        >
          {{ savingComment ? 'Saving…' : 'Add pending comment' }}
        </button>
      </div>
    </form>

    <ReviewTray
      :reviews="reviews"
      :draft="draft"
      :open="trayOpen"
      :overall-note="overallNote"
      :summary-saving="summarySaving || summaryDirty"
      :acknowledge-outdated="acknowledgeOutdated"
      :error="trayError"
      :layout-busy="false"
      :submitting="submitting"
      :discarding="discarding"
      :delivery-errors="deliveryErrors"
      subject-label="changes"
      :discard-action="discardDraft"
      @update:open="trayOpen = $event"
      @update:overall-note="editOverall"
      @update:acknowledge-outdated="acknowledgeOutdated = $event"
      @navigate="navigate"
      @focus-comment="activeComment = $event"
      @save-overall="saveOverall"
      @retarget="retarget"
      @submit="submit"
      @retry="retryDelivery"
    />
    <p v-if="notice" class="absolute bottom-1 left-3 text-2xs text-accent" role="status">
      {{ notice }}
    </p>
  </section>
</template>
