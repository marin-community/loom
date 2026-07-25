<script setup lang="ts">
import {
  computed,
  h,
  isVNode,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
  type VNode,
} from 'vue';
import { useRouter } from 'vue-router';
import { renderTokens } from '../markdown-render';
import { useMarkdownDoc, routeDocLink } from '../lib/markdownDoc';
import type { IssueRefStatus, Review, ReviewComment } from '../types';
import {
  addReviewComment,
  createArtifactReview,
  deleteReviewComment,
  discardReview,
  listArtifactReviews,
  retryReviewDelivery,
  resolveThread,
  setReviewCommentResolution,
  submitReview,
  updateReviewComment,
} from '../api';
import {
  captureAnchor,
  locate,
  blockContaining,
  paintHighlights,
  clearHighlights,
  COMMENT_UI_ATTR,
  type TextAnchor,
} from '../discussion-anchor';
import ReviewCommentCard from './ReviewCommentCard.vue';

// Dock/pop swaps mounts of the same document. Keep a small in-memory scroll
// ledger keyed by session + artifact + revision so the document remains where
// the reader left it across that layout change.
const scrollPositions = new Map<string, number>();
const MAX_SCROLL_POSITIONS = 100;
const LAST_SCROLL_POSITION = 'loom.artifactScrollPosition';

const props = defineProps<{
  id: string;
  path: string;
  source: string;
  refs?: Record<string, IssueRefStatus>;
  artifactName: string;
  /** The artifact revision currently rendered, which may be historical. */
  rev: number;
}>();

const router = useRouter();

// --- markdown body + scroll ownership --------------------------------------

const containerEl = ref<HTMLElement | null>(null);
const scrollerEl = ref<HTMLElement | null>(null);
let activeScrollKey = '';

function scrollKey(): string {
  return `${props.id}:${props.artifactName}:${props.rev}`;
}

function cacheScroll(key = activeScrollKey): number | undefined {
  if (!key || !scrollerEl.value) return;
  const top = scrollerEl.value.scrollTop;
  if (!scrollPositions.has(key) && scrollPositions.size >= MAX_SCROLL_POSITIONS) {
    const oldest = scrollPositions.keys().next().value;
    if (oldest) scrollPositions.delete(oldest);
  }
  scrollPositions.set(key, top);
  return top;
}

function persistScroll(key = activeScrollKey): void {
  const top = cacheScroll(key);
  if (top === undefined) return;
  try {
    sessionStorage.setItem(LAST_SCROLL_POSITION, JSON.stringify({ key, top }));
  } catch {
    // The in-module ledger still covers ordinary dock/pop swaps.
  }
}

async function restoreScroll(key: string): Promise<void> {
  await nextTick();
  if (key !== activeScrollKey || !scrollerEl.value) return;
  let top = scrollPositions.get(key);
  if (top === undefined) {
    try {
      const last = JSON.parse(sessionStorage.getItem(LAST_SCROLL_POSITION) ?? 'null') as {
        key?: string;
        top?: number;
      } | null;
      if (last?.key === key && typeof last.top === 'number') top = last.top;
    } catch {
      // Invalid tab-local state is equivalent to no saved position.
    }
  }
  scrollerEl.value.scrollTop = top ?? 0;
}

watch(
  () => [props.id, props.artifactName, props.rev] as const,
  (_next, previous) => {
    if (previous) persistScroll(`${previous[0]}:${previous[1]}:${previous[2]}`);
    activeScrollKey = scrollKey();
    void restoreScroll(activeScrollKey);
    void loadReviews();
  },
);

const { body, error, tokens, ctx } = useMarkdownDoc(props, () => {
  locateCycle();
  if (activeScrollKey) void restoreScroll(activeScrollKey);
});

// --- review state -----------------------------------------------------------

type ReviewEntry = { review: Review; comment: ReviewComment };

const reviews = ref<Review[]>([]);
const reviewError = ref('');
const reviewNotice = ref('');
const activeId = ref<number | null>(null);
const trayOpen = ref(false);
const overallNote = ref('');
const acknowledgeOutdated = ref(false);
const submitting = ref(false);
const discardConfirm = ref(false);
const reanchorCommentId = ref<number | null>(null);

const draft = computed(() => reviews.value.find((review) => review.status === 'draft') ?? null);
const recentSubmitted = computed(
  () =>
    [...reviews.value]
      .filter((review) => review.status === 'submitted' && !review.legacy)
      .sort((a, b) => b.id - a.id)[0] ?? null,
);
const draftEntries = computed<ReviewEntry[]>(() =>
  (draft.value?.comments ?? []).map((comment) => ({ review: draft.value!, comment })),
);
const feedbackPreview = computed(() => {
  const item = draft.value;
  if (!item) return '';
  let message = `The user submitted feedback on artifact \`${item.subject.label}\`, revision ${item.subject.version}.`;
  if (overallNote.value.trim()) message += `\n\nOverall:\n${overallNote.value.trim()}`;
  item.comments.forEach((comment, index) => {
    const quote = String(comment.anchor.quote ?? '').replaceAll('\n', ' ');
    message += `\n\n${index + 1}. Revision ${comment.subject_version}, ${comment.anchor_kind} anchor`;
    if (quote) message += ` “${quote}”`;
    message += `:\n${comment.body}`;
  });
  message += `\n\n[review_id: ${item.id}; delivery_key: ${item.delivery_key}]`;
  return message;
});
const allEntries = computed<ReviewEntry[]>(() => {
  const entries: ReviewEntry[] = [];
  for (const review of reviews.value) {
    if (review.legacy) {
      const first = review.comments[0];
      if (first) entries.push({ review, comment: first });
    } else {
      for (const comment of review.comments) entries.push({ review, comment });
    }
  }
  return entries;
});

function replaceReview(next: Review) {
  const index = reviews.value.findIndex((review) => review.id === next.id);
  if (index === -1) reviews.value = [next, ...reviews.value];
  else {
    const copy = [...reviews.value];
    copy[index] = next;
    reviews.value = copy;
  }
}

async function loadReviews() {
  try {
    reviews.value = await listArtifactReviews(props.id, props.artifactName);
    reviewError.value = '';
    if (activeId.value != null && !allEntries.value.some((x) => x.comment.id === activeId.value)) {
      activeId.value = null;
    }
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
  await nextTick();
  locateCycle();
}

// --- locate + paint + group -------------------------------------------------

const cardsByBlock = ref<Map<number, ReviewEntry[]>>(new Map());
const locatedEntries = ref<{ entry: ReviewEntry; range: Range }[]>([]);
const orphaned = ref<ReviewEntry[]>([]);

const pending = ref<{ anchor: TextAnchor & { block_index?: number | null } } | null>(null);
let pendingRange: Range | null = null;
const pendingBlock = ref(-1);
const pendingDraft = ref('');
const savingComment = ref(false);

function locateCycle() {
  const root = body.value;
  if (!root) {
    cardsByBlock.value = new Map();
    locatedEntries.value = [];
    orphaned.value = allEntries.value;
    clearHighlights();
    return;
  }
  const located: { entry: ReviewEntry; range: Range; block: number }[] = [];
  const unlocated: ReviewEntry[] = [];
  for (const entry of allEntries.value) {
    const range = locate(root, entry.comment.anchor);
    if (!range) {
      unlocated.push(entry);
      continue;
    }
    const element =
      blockContaining(root, range.endContainer) ?? blockContaining(root, range.startContainer);
    const attr = element?.getAttribute('data-block');
    const capturedBlock = entry.comment.anchor.block_index;
    const block =
      attr != null ? Number(attr) : typeof capturedBlock === 'number' ? capturedBlock : -1;
    located.push({ entry, range, block });
  }
  locatedEntries.value = located.map(({ entry, range }) => ({ entry, range }));

  const activeRange =
    located.find(({ entry }) => entry.comment.id === activeId.value)?.range ?? null;
  paintHighlights(
    located.map(({ range }) => range),
    activeRange,
  );

  located.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));
  const byBlock = new Map<number, ReviewEntry[]>();
  for (const { entry, block } of located) {
    const entries = byBlock.get(block);
    if (entries) entries.push(entry);
    else byBlock.set(block, [entry]);
  }
  cardsByBlock.value = byBlock;
  orphaned.value = unlocated;

  if (pending.value && pendingRange && root.contains(pendingRange.endContainer)) {
    const element = blockContaining(root, pendingRange.endContainer);
    const attr = element?.getAttribute('data-block');
    pendingBlock.value = attr != null ? Number(attr) : -1;
    pending.value.anchor.block_index = pendingBlock.value;
  } else if (!pending.value) {
    pendingBlock.value = -1;
  }
}

watch(activeId, locateCycle);
watch(allEntries, () => nextTick(locateCycle));

// --- keyboard/mouse selection affordance -----------------------------------

const selectionButton = ref<{
  anchor: TextAnchor & { block_index?: number | null };
  top: number;
  left: number;
} | null>(null);
const buttonEl = ref<HTMLElement | null>(null);

function updateSelectionButton() {
  const root = body.value;
  const selection = window.getSelection();
  if (!root || !selection || selection.rangeCount === 0 || selection.isCollapsed) {
    selectionButton.value = null;
    return;
  }
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) {
    selectionButton.value = null;
    return;
  }
  const start = range.startContainer;
  if ((start instanceof Element ? start : start.parentElement)?.closest(`[${COMMENT_UI_ATTR}]`)) {
    selectionButton.value = null;
    return;
  }
  const anchor = captureAnchor(root, range) as
    (TextAnchor & { block_index?: number | null }) | null;
  if (!anchor) {
    selectionButton.value = null;
    return;
  }
  const element =
    blockContaining(root, range.endContainer) ?? blockContaining(root, range.startContainer);
  const attr = element?.getAttribute('data-block');
  anchor.block_index = attr != null ? Number(attr) : null;
  const container = containerEl.value;
  if (!container) return;
  const containerRect = container.getBoundingClientRect();
  const rect = range.getBoundingClientRect();
  selectionButton.value = {
    anchor,
    top: rect.bottom - containerRect.top + 4,
    left: Math.max(0, rect.right - containerRect.left - 150),
  };
}

function onMouseUp() {
  updateSelectionButton();
}

function onSelectionChange() {
  updateSelectionButton();
}

function onDocMouseDown(event: MouseEvent) {
  const target = event.target as Node;
  if (buttonEl.value?.contains(target)) return;
  selectionButton.value = null;
}

async function useSelection() {
  if (!selectionButton.value) return;
  const selection = window.getSelection();
  const range = selection?.rangeCount ? selection.getRangeAt(0).cloneRange() : null;
  const anchor = selectionButton.value.anchor;
  selectionButton.value = null;

  if (reanchorCommentId.value != null && draft.value) {
    try {
      const updated = await updateReviewComment(draft.value.id, reanchorCommentId.value, {
        subject_version: String(props.rev),
        anchor_kind: 'text',
        anchor,
      });
      const review = { ...draft.value };
      review.comments = review.comments.map((comment) =>
        comment.id === updated.id ? updated : comment,
      );
      replaceReview(review);
      activeId.value = updated.id;
      reanchorCommentId.value = null;
      reviewNotice.value = `Comment re-anchored to revision ${props.rev}.`;
      selection?.removeAllRanges();
      await nextTick();
      locateCycle();
    } catch (e) {
      reviewError.value = (e as Error).message;
    }
    return;
  }

  pendingRange = range;
  pending.value = { anchor };
  pendingDraft.value = '';
  selection?.removeAllRanges();
  locateCycle();
  await nextTick();
  containerEl.value
    ?.querySelector<HTMLTextAreaElement>('[data-testid="review-comment-composer"] textarea')
    ?.focus();
}

// --- links + click-to-focus -------------------------------------------------

function onArticleClick(event: MouseEvent) {
  if (routeDocLink(event, router, body.value)) return;
  const selection = window.getSelection();
  if (selection && !selection.isCollapsed) return;
  const doc = document as Document & {
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  let node: Node | null = null;
  let offset = 0;
  if (doc.caretRangeFromPoint) {
    const range = doc.caretRangeFromPoint(event.clientX, event.clientY);
    if (range) {
      node = range.startContainer;
      offset = range.startOffset;
    }
  } else if (doc.caretPositionFromPoint) {
    const position = doc.caretPositionFromPoint(event.clientX, event.clientY);
    if (position) {
      node = position.offsetNode;
      offset = position.offset;
    }
  }
  if (!node) return;
  const hit = locatedEntries.value.find(({ range }) => {
    try {
      return range.isPointInRange(node as Node, offset);
    } catch {
      return false;
    }
  });
  if (hit) focusComment(hit.entry.comment.id);
}

function focusComment(commentId: number) {
  activeId.value = activeId.value === commentId ? null : commentId;
  if (activeId.value == null) return;
  const located = locatedEntries.value.find((entry) => entry.entry.comment.id === commentId);
  const scroller = scrollerEl.value;
  if (!located || !scroller) return;
  const scrollerRect = scroller.getBoundingClientRect();
  const rect = located.range.getBoundingClientRect();
  if (rect.top >= scrollerRect.top && rect.bottom <= scrollerRect.bottom) return;
  const element =
    located.range.startContainer.nodeType === Node.TEXT_NODE
      ? located.range.startContainer.parentElement
      : (located.range.startContainer as Element);
  element?.scrollIntoView({ block: 'center' });
}

function navigateDraft(direction: number) {
  const entries = draftEntries.value;
  if (!entries.length) return;
  const current = entries.findIndex((entry) => entry.comment.id === activeId.value);
  const next = (current + direction + entries.length) % entries.length;
  activeId.value = null;
  nextTick(() => focusComment(entries[next].comment.id));
}

// --- draft mutation ---------------------------------------------------------

async function ensureDraft(): Promise<Review> {
  if (draft.value) return draft.value;
  const created = await createArtifactReview(props.id, {
    subject_kind: 'artifact',
    subject_key: props.artifactName,
    subject_version: String(props.rev),
  });
  replaceReview(created);
  return created;
}

async function createPendingComment() {
  const text = pendingDraft.value.trim();
  if (!pending.value || !text || savingComment.value) return;
  savingComment.value = true;
  reviewError.value = '';
  try {
    const review = await ensureDraft();
    const comment = await addReviewComment(review.id, {
      subject_version: String(props.rev),
      anchor_kind: 'text',
      anchor: pending.value.anchor,
      body: text,
    });
    const updated = { ...review, comments: [...review.comments, comment] };
    // The current version is returned when the envelope is created. Existing
    // drafts are refreshed by list/revision events.
    updated.outdated =
      updated.subject.version !== updated.subject.current_version ||
      updated.comments.some((item) => item.subject_version !== updated.subject.current_version);
    replaceReview(updated);
    activeId.value = comment.id;
    pending.value = null;
    pendingRange = null;
    pendingDraft.value = '';
    trayOpen.value = true;
    reviewNotice.value = 'Pending comment saved. Submit the review when your feedback is complete.';
    await nextTick();
    locateCycle();
  } catch (e) {
    reviewError.value = (e as Error).message;
  } finally {
    savingComment.value = false;
  }
}

function cancelPendingComment() {
  pending.value = null;
  pendingRange = null;
  pendingDraft.value = '';
  locateCycle();
}

async function editComment(payload: { commentId: number; body: string }) {
  if (!draft.value) return;
  try {
    const updated = await updateReviewComment(draft.value.id, payload.commentId, {
      body: payload.body,
    });
    replaceReview({
      ...draft.value,
      comments: draft.value.comments.map((comment) =>
        comment.id === updated.id ? updated : comment,
      ),
    });
    reviewNotice.value = 'Pending comment updated.';
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
}

async function removeComment(commentId: number) {
  if (!draft.value) return;
  try {
    await deleteReviewComment(draft.value.id, commentId);
    replaceReview({
      ...draft.value,
      comments: draft.value.comments.filter((comment) => comment.id !== commentId),
    });
    if (activeId.value === commentId) activeId.value = null;
    if (reanchorCommentId.value === commentId) reanchorCommentId.value = null;
    reviewNotice.value = 'Pending comment deleted.';
    await nextTick();
    locateCycle();
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
}

async function setResolution(review: Review, commentId: number, resolved: boolean) {
  try {
    if (review.legacy) {
      if (!resolved) return;
      await resolveThread(props.id, props.artifactName, -review.id);
      await loadReviews();
    } else {
      const updated = await setReviewCommentResolution(review.id, commentId, resolved);
      replaceReview({
        ...review,
        comments: review.comments.map((comment) => (comment.id === updated.id ? updated : comment)),
      });
    }
    reviewNotice.value = resolved ? 'Review comment resolved.' : 'Review comment reopened.';
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
}

function beginReanchor(commentId: number) {
  reanchorCommentId.value = commentId;
  activeId.value = commentId;
  reviewNotice.value = 'Select replacement text in the artifact.';
}

async function discardDraft() {
  if (!draft.value) return;
  try {
    const reviewId = draft.value.id;
    await discardReview(reviewId);
    reviews.value = reviews.value.filter((review) => review.id !== reviewId);
    activeId.value = null;
    reanchorCommentId.value = null;
    overallNote.value = '';
    acknowledgeOutdated.value = false;
    discardConfirm.value = false;
    trayOpen.value = false;
    reviewNotice.value = 'Draft review discarded.';
    await nextTick();
    locateCycle();
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
}

async function submitDraft() {
  if (!draft.value || submitting.value) return;
  submitting.value = true;
  reviewError.value = '';
  try {
    const submitted = await submitReview(draft.value.id, {
      summary: overallNote.value,
      acknowledge_outdated: acknowledgeOutdated.value,
    });
    replaceReview(submitted);
    activeId.value = null;
    reanchorCommentId.value = null;
    overallNote.value = '';
    acknowledgeOutdated.value = false;
    reviewNotice.value =
      submitted.delivery_state === 'delivered'
        ? 'Review submitted to the conversation.'
        : 'Review submitted and queued for the conversation.';
    await nextTick();
    locateCycle();
  } catch (e) {
    reviewError.value = (e as Error).message;
  } finally {
    submitting.value = false;
  }
}

async function retryDelivery() {
  if (!recentSubmitted.value) return;
  try {
    const updated = await retryReviewDelivery(recentSubmitted.value.id);
    replaceReview(updated);
    reviewNotice.value =
      updated.delivery_state === 'delivered'
        ? 'Review delivered to the conversation.'
        : 'Review delivery retry queued.';
  } catch (e) {
    reviewError.value = (e as Error).message;
  }
}

// --- SSE forwarding ---------------------------------------------------------

async function onCommentEvent(
  kind: string,
  data: { artifact?: string; subject_key?: string; session_id?: string },
): Promise<void> {
  if (data.artifact && data.artifact !== props.artifactName) return;
  if (data.session_id && data.session_id !== props.id) return;
  if (
    ![
      'comment_added',
      'comment_resolved',
      'review_draft_changed',
      'review_submitted',
      'review_delivery',
      'review_comment_resolved',
    ].includes(kind)
  ) {
    return;
  }
  await loadReviews();
}

function snapshotScroll(): void {
  persistScroll();
}

defineExpose({ onCommentEvent, snapshotScroll });

// --- lifecycle --------------------------------------------------------------

onMounted(() => {
  activeScrollKey = scrollKey();
  void restoreScroll(activeScrollKey);
  document.addEventListener('selectionchange', onSelectionChange);
  document.addEventListener('mousedown', onDocMouseDown, true);
  void loadReviews();
});

onBeforeUnmount(() => {
  persistScroll();
  document.removeEventListener('selectionchange', onSelectionChange);
  document.removeEventListener('mousedown', onDocMouseDown, true);
  clearHighlights();
});

// --- interleaved render -----------------------------------------------------

function stop<T extends Event>(fn: () => void) {
  return (event: T) => {
    event.stopPropagation();
    fn();
  };
}

function renderCard(entry: ReviewEntry): VNode {
  return h(
    'div',
    { [COMMENT_UI_ATTR]: '', key: `review-${entry.review.id}-${entry.comment.id}` },
    h(ReviewCommentCard, {
      review: entry.review,
      comment: entry.comment,
      active: entry.comment.id === activeId.value,
      reanchoring: entry.comment.id === reanchorCommentId.value,
      onFocus: focusComment,
      onEdit: editComment,
      onDelete: removeComment,
      onReanchor: beginReanchor,
      onCancelReanchor: () => {
        reanchorCommentId.value = null;
      },
      onResolution: (commentId: number, resolved: boolean) =>
        setResolution(entry.review, commentId, resolved),
    }),
  );
}

function renderComposer(): VNode {
  return h(
    'div',
    {
      key: 'pending-review-comment',
      [COMMENT_UI_ATTR]: '',
      class: 'my-2 rounded border border-accent bg-subtle/50 p-2 text-xs ring-1 ring-accent/40',
      'data-testid': 'review-comment-composer',
      onClick: (event: Event) => event.stopPropagation(),
    },
    [
      h(
        'div',
        { class: 'mb-1 text-2xs font-semibold uppercase tracking-wide text-accent' },
        'Pending review comment',
      ),
      h('textarea', {
        value: pendingDraft.value,
        rows: 3,
        placeholder: 'Leave feedback for this selection…',
        class:
          'w-full resize-y rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent',
        onInput: (event: Event) => {
          pendingDraft.value = (event.target as HTMLTextAreaElement).value;
        },
        onKeydown: (event: KeyboardEvent) => {
          if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
            event.preventDefault();
            void createPendingComment();
          }
        },
        onClick: (event: Event) => event.stopPropagation(),
        onMousedown: (event: Event) => event.stopPropagation(),
      }),
      h('div', { class: 'mt-1.5 flex items-center gap-1.5' }, [
        h(
          'button',
          {
            type: 'button',
            class: 'btn-primary px-2 py-1 text-2xs',
            disabled: savingComment.value,
            onClick: stop(() => void createPendingComment()),
          },
          savingComment.value ? 'Saving…' : 'Add pending comment',
        ),
        h(
          'button',
          {
            type: 'button',
            class: 'btn-secondary px-2 py-1 text-2xs',
            onClick: stop(cancelPendingComment),
          },
          'Cancel',
        ),
      ]),
    ],
  );
}

const DocBody = () => {
  const renderContext = ctx.value;
  if (!renderContext) return null;
  const blocks = renderTokens(tokens.value, renderContext);
  const output: (VNode | string)[] = [];
  const placed = new Set<number>();
  blocks.forEach((block, index) => {
    output.push(isVNode(block) ? block : String(block));
    for (const entry of cardsByBlock.value.get(index) ?? []) output.push(renderCard(entry));
    if (pending.value && pendingBlock.value === index) output.push(renderComposer());
    placed.add(index);
  });
  for (const [block, entries] of cardsByBlock.value) {
    if (!placed.has(block)) for (const entry of entries) output.push(renderCard(entry));
  }
  if (pending.value && !placed.has(pendingBlock.value)) output.push(renderComposer());
  if (orphaned.value.length) {
    output.push(
      h(
        'section',
        {
          key: 'stale-review-anchors',
          [COMMENT_UI_ATTR]: '',
          class: 'my-4 border-t border-line pt-3',
          'data-testid': 'review-stale-anchors',
        },
        [
          h(
            'h3',
            { class: 'mb-2 text-xs font-semibold text-block' },
            `Stale anchors (${orphaned.value.length})`,
          ),
          ...orphaned.value.map(renderCard),
        ],
      ),
    );
  }
  return output;
};
</script>

<template>
  <div ref="containerEl" class="relative h-full min-h-0 w-full overflow-hidden">
    <div
      ref="scrollerEl"
      class="h-full min-h-0 w-full overflow-auto bg-surface"
      data-testid="artifact-scroll"
      @scroll.passive="cacheScroll()"
    >
      <p
        v-if="error"
        class="m-4 rounded border border-block-line bg-block-soft p-3 text-sm text-block"
      >
        {{ error }}
      </p>
      <p
        v-if="reviewError"
        class="mx-auto mt-3 max-w-3xl rounded border border-block-line bg-block-soft px-3 py-2 text-xs text-block"
        data-testid="review-error"
      >
        {{ reviewError }}
      </p>
      <article
        ref="body"
        class="markdown-body mx-auto max-w-3xl px-6 pb-32 pt-5"
        @click="onArticleClick"
        @mouseup="onMouseUp"
      >
        <DocBody />
      </article>
    </div>

    <button
      v-if="selectionButton"
      ref="buttonEl"
      type="button"
      class="btn-primary absolute z-30 gap-1 px-2 py-1 text-xs shadow-sm"
      data-testid="review-selection-button"
      :aria-label="
        reanchorCommentId == null
          ? 'Add pending review comment to selection'
          : 'Re-anchor pending comment to selection'
      "
      :style="{ top: selectionButton.top + 'px', left: selectionButton.left + 'px' }"
      @mousedown.prevent
      @click="useSelection"
    >
      {{ reanchorCommentId == null ? '＋ Add comment' : '↪ Re-anchor selection' }}
    </button>

    <aside
      v-if="draft || recentSubmitted"
      class="absolute bottom-3 right-3 z-20 w-[min(28rem,calc(100%-1.5rem))] rounded-lg border border-line bg-surface shadow-xl"
      data-testid="review-tray"
      aria-label="Review tray"
    >
      <div class="flex min-h-10 items-center gap-1.5 px-2">
        <button
          type="button"
          class="min-w-0 flex-1 px-1 py-2 text-left text-xs font-semibold text-fg"
          data-testid="review-tray-toggle"
          :aria-expanded="trayOpen"
          @click="trayOpen = !trayOpen"
        >
          <template v-if="draft">
            Review · {{ draft.comments.length }} pending
            <span v-if="draft.outdated" class="ml-1 text-block">· stale</span>
          </template>
          <template v-else> Review submitted · {{ recentSubmitted?.delivery_state }} </template>
        </button>
        <template v-if="draft?.comments.length">
          <button
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            aria-label="Previous pending comment"
            @click="navigateDraft(-1)"
          >
            ↑
          </button>
          <button
            type="button"
            class="btn-secondary px-2 py-1 text-xs"
            aria-label="Next pending comment"
            @click="navigateDraft(1)"
          >
            ↓
          </button>
        </template>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          :aria-label="trayOpen ? 'Collapse review tray' : 'Expand review tray'"
          @click="trayOpen = !trayOpen"
        >
          {{ trayOpen ? '▾' : '▴' }}
        </button>
      </div>

      <div v-if="trayOpen" class="max-h-[min(60vh,34rem)] overflow-auto border-t border-line p-3">
        <template v-if="draft">
          <p
            v-if="draft.outdated"
            class="mb-3 rounded border border-block-line bg-block-soft p-2 text-xs text-block"
            data-testid="review-stale-warning"
          >
            This artifact is now revision {{ draft.subject.current_version }}. Stale anchors are
            preserved; re-anchor them, or acknowledge the older context before submitting.
          </p>

          <div class="space-y-1.5" aria-label="Pending comments">
            <button
              v-for="(entry, index) in draftEntries"
              :key="entry.comment.id"
              type="button"
              class="flex w-full items-center gap-2 rounded border border-line px-2 py-1.5 text-left text-xs hover:border-accent"
              @click="focusComment(entry.comment.id)"
            >
              <span class="shrink-0 font-mono text-2xs text-faint">{{ index + 1 }}</span>
              <span class="min-w-0 flex-1 truncate text-fg">{{ entry.comment.body }}</span>
              <span
                v-if="entry.comment.subject_version !== draft.subject.current_version"
                class="shrink-0 text-2xs text-block"
              >
                rev {{ entry.comment.subject_version }}
              </span>
            </button>
          </div>

          <label class="mt-3 block text-xs font-medium text-muted" for="review-overall-note">
            Overall note <span class="font-normal text-faint">(optional)</span>
          </label>
          <textarea
            id="review-overall-note"
            v-model="overallNote"
            rows="3"
            class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
            placeholder="Feedback that applies to the artifact as a whole…"
            data-testid="review-overall-note"
          ></textarea>

          <label
            v-if="draft.outdated"
            class="mt-2 flex cursor-pointer items-start gap-2 rounded bg-subtle/50 p-2 text-xs text-muted"
          >
            <input
              v-model="acknowledgeOutdated"
              type="checkbox"
              class="mt-0.5"
              data-testid="review-stale-ack"
            />
            Submit against the captured revision context intentionally.
          </label>

          <div class="mt-3 border-t border-line pt-3">
            <p class="mb-2 text-2xs font-semibold uppercase tracking-wide text-faint">
              Conversation feedback preview
            </p>
            <pre
              class="max-h-32 overflow-auto whitespace-pre-wrap rounded bg-code p-2 text-xs text-code-fg"
              >{{ feedbackPreview }}</pre>
          </div>

          <div class="mt-3 flex flex-wrap items-center gap-2">
            <template v-if="discardConfirm">
              <span class="text-xs text-block">Discard all pending comments?</span>
              <button type="button" class="btn-danger px-2 py-1 text-xs" @click="discardDraft">
                Discard
              </button>
              <button
                type="button"
                class="btn-secondary px-2 py-1 text-xs"
                @click="discardConfirm = false"
              >
                Cancel
              </button>
            </template>
            <button
              v-else
              type="button"
              class="btn-secondary px-2 py-1 text-xs text-block"
              @click="discardConfirm = true"
            >
              Discard draft
            </button>
            <button
              type="button"
              class="btn-primary ml-auto px-3 py-1.5 text-xs"
              data-testid="submit-review"
              :disabled="
                submitting ||
                (!draft.comments.length && !overallNote.trim()) ||
                (draft.outdated && !acknowledgeOutdated)
              "
              @click="submitDraft"
            >
              {{ submitting ? 'Submitting…' : 'Submit review' }}
            </button>
          </div>
        </template>

        <template v-else-if="recentSubmitted">
          <div class="text-xs text-muted">
            <p class="font-medium text-fg">Review {{ recentSubmitted.id }} submitted</p>
            <p class="mt-1">
              Delivery:
              <span
                :class="recentSubmitted.delivery_state === 'failed' ? 'text-block' : 'text-accent'"
                >{{ recentSubmitted.delivery_state }}</span
              >
            </p>
            <p
              v-if="recentSubmitted.delivery_error"
              class="mt-2 rounded bg-block-soft p-2 text-block"
            >
              {{ recentSubmitted.delivery_error }}
            </p>
            <button
              v-if="recentSubmitted.delivery_state === 'failed'"
              type="button"
              class="btn-primary mt-2 px-2 py-1 text-xs"
              @click="retryDelivery"
            >
              Retry delivery
            </button>
          </div>
        </template>
      </div>
    </aside>

    <div
      v-if="reviewNotice"
      class="pointer-events-none absolute bottom-1 left-1/2 z-40 -translate-x-1/2 rounded bg-fg px-2 py-1 text-2xs text-surface opacity-90"
      aria-live="polite"
    >
      {{ reviewNotice }}
    </div>
  </div>
</template>
