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
  updateReview,
  updateReviewComment,
  ApiError,
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
type ScrollSnapshot = { top: number; block?: number; offset?: number };
const scrollPositions = new Map<string, ScrollSnapshot>();
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

function cacheScroll(key = activeScrollKey): ScrollSnapshot | undefined {
  if (!key || !scrollerEl.value) return;
  const scroller = scrollerEl.value;
  const viewportTop = scroller.getBoundingClientRect().top;
  const visible = [...scroller.querySelectorAll<HTMLElement>('[data-block]')].find(
    (element) => element.getBoundingClientRect().bottom > viewportTop,
  );
  const snapshot: ScrollSnapshot = { top: scroller.scrollTop };
  const block = visible?.getAttribute('data-block');
  if (visible && block != null) {
    snapshot.block = Number(block);
    snapshot.offset = visible.getBoundingClientRect().top - viewportTop;
  }
  if (!scrollPositions.has(key) && scrollPositions.size >= MAX_SCROLL_POSITIONS) {
    const oldest = scrollPositions.keys().next().value;
    if (oldest) scrollPositions.delete(oldest);
  }
  scrollPositions.set(key, snapshot);
  return snapshot;
}

function persistScroll(key = activeScrollKey): void {
  const snapshot = cacheScroll(key);
  if (snapshot === undefined) return;
  try {
    sessionStorage.setItem(LAST_SCROLL_POSITION, JSON.stringify({ key, ...snapshot }));
  } catch {
    // The in-module ledger still covers ordinary dock/pop swaps.
  }
}

async function restoreScroll(key: string): Promise<void> {
  await nextTick();
  if (key !== activeScrollKey || !scrollerEl.value) return;
  let snapshot = scrollPositions.get(key);
  if (snapshot === undefined) {
    try {
      const last = JSON.parse(sessionStorage.getItem(LAST_SCROLL_POSITION) ?? 'null') as {
        key?: string;
        top?: number;
        block?: number;
        offset?: number;
      } | null;
      if (last?.key === key && typeof last.top === 'number') {
        snapshot = { top: last.top, block: last.block, offset: last.offset };
      }
    } catch {
      // Invalid tab-local state is equivalent to no saved position.
    }
  }
  const scroller = scrollerEl.value;
  scroller.scrollTop = snapshot?.top ?? 0;
  if (snapshot?.block != null && snapshot.offset != null) {
    const sentinel = scroller.querySelector<HTMLElement>(`[data-block="${snapshot.block}"]`);
    if (sentinel) {
      scroller.scrollTop +=
        sentinel.getBoundingClientRect().top -
        scroller.getBoundingClientRect().top -
        snapshot.offset;
    }
  }
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
const composerError = ref('');
const trayError = ref('');
const commentErrors = ref<Record<number, string>>({});
const activeId = ref<number | null>(null);
const trayOpen = ref(false);
const overallNote = ref('');
const summarySaving = ref(false);
const summaryHydrating = ref(false);
const summaryDirty = ref(false);
let summarySaveTimer: ReturnType<typeof setTimeout> | null = null;
let summarySavePromise: Promise<Review | null> | null = null;
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
const failedSubmissions = computed(() =>
  [...reviews.value]
    .filter(
      (review) =>
        review.status === 'submitted' && !review.legacy && review.delivery_state === 'failed',
    )
    .sort((a, b) => b.id - a.id),
);
const draftEntries = computed<ReviewEntry[]>(() =>
  (draft.value?.comments ?? []).map((comment) => ({ review: draft.value!, comment })),
);
const feedbackPreview = computed(() => {
  return draft.value?.message ?? '';
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

function hydrateSummary(next: Review | null, force = false) {
  if (summaryDirty.value && !force) return;
  summaryHydrating.value = true;
  overallNote.value = next?.summary ?? '';
  summaryDirty.value = false;
  nextTick(() => {
    summaryHydrating.value = false;
  });
}

function conflictReview(error: unknown): Review | null {
  if (!(error instanceof ApiError) || error.status !== 409) return null;
  const details = error.body.details;
  if (!details || typeof details !== 'object') return null;
  const fresh = (details as { review?: unknown }).review;
  if (!fresh || typeof fresh !== 'object' || typeof (fresh as Review).id !== 'number') return null;
  const review = fresh as Review;
  replaceReview(review);
  hydrateSummary(review.status === 'draft' ? review : null, true);
  return review;
}

function mutationMessage(error: unknown): string {
  if (conflictReview(error)) {
    return 'This draft changed elsewhere. The latest review is loaded; review it before retrying.';
  }
  return (error as Error).message;
}

async function loadReviews() {
  try {
    reviews.value = await listArtifactReviews(props.id, props.artifactName);
    hydrateSummary(reviews.value.find((review) => review.status === 'draft') ?? null);
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

watch(overallNote, () => {
  if (summaryHydrating.value) return;
  summaryDirty.value = true;
  if (!draft.value) return;
  if (summarySaveTimer) clearTimeout(summarySaveTimer);
  summarySaveTimer = setTimeout(() => {
    summarySaveTimer = null;
    void saveOverallNote();
  }, 400);
});

// --- locate + paint + group -------------------------------------------------

const cardsByBlock = ref<Map<number, ReviewEntry[]>>(new Map());
const locatedEntries = ref<{ entry: ReviewEntry; range: Range }[]>([]);
const orphaned = ref<ReviewEntry[]>([]);

const pending = ref<{
  anchor: TextAnchor & { block_index?: number | null };
  subjectVersion: string;
} | null>(null);
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
  subjectVersion: string;
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
    subjectVersion: String(props.rev),
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
  const subjectVersion = selectionButton.value.subjectVersion;
  selectionButton.value = null;

  if (reanchorCommentId.value != null && draft.value) {
    const commentId = reanchorCommentId.value;
    commentErrors.value[commentId] = '';
    try {
      if (!(await saveOverallNote())) return;
      if (!draft.value) return;
      const updated = await updateReviewComment(draft.value.id, commentId, {
        expected_revision: draft.value.draft_revision,
        subject_version: subjectVersion,
        anchor_kind: 'text',
        anchor,
      });
      replaceReview(updated);
      activeId.value = commentId;
      reanchorCommentId.value = null;
      reviewNotice.value =
        updated.subject.version === subjectVersion
          ? `Comment re-anchored; review target is now revision ${subjectVersion}.`
          : `Comment re-anchored to revision ${subjectVersion}; older anchors still remain.`;
      selection?.removeAllRanges();
      await nextTick();
      locateCycle();
    } catch (e) {
      commentErrors.value[commentId] = mutationMessage(e);
    }
    return;
  }

  pendingRange = range;
  pending.value = { anchor, subjectVersion };
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

async function focusComment(commentId: number) {
  activeId.value = commentId;
  await nextTick();
  const located = locatedEntries.value.find((entry) => entry.entry.comment.id === commentId);
  const scroller = scrollerEl.value;
  if (!scroller) return;
  if (!located) {
    const card = scroller.querySelector<HTMLElement>(`[data-review-card="${commentId}"]`);
    card?.scrollIntoView({ block: 'center' });
    card?.focus();
    return;
  }
  const scrollerRect = scroller.getBoundingClientRect();
  const rect = located.range.getBoundingClientRect();
  if (rect.top < scrollerRect.top || rect.bottom > scrollerRect.bottom) {
    const element =
      located.range.startContainer.nodeType === Node.TEXT_NODE
        ? located.range.startContainer.parentElement
        : (located.range.startContainer as Element);
    element?.scrollIntoView({ block: 'center' });
  }
  scroller.querySelector<HTMLElement>(`[data-review-card="${commentId}"]`)?.focus();
}

async function closeComment(commentId: number) {
  if (activeId.value === commentId) activeId.value = null;
  await nextTick();
  scrollerEl.value?.querySelector<HTMLElement>(`[data-review-collapsed="${commentId}"]`)?.focus();
}

function navigateDraft(direction: number) {
  const entries = draftEntries.value;
  if (!entries.length) return;
  const current = entries.findIndex((entry) => entry.comment.id === activeId.value);
  const next = (current + direction + entries.length) % entries.length;
  void focusComment(entries[next].comment.id);
}

// --- draft mutation ---------------------------------------------------------

async function ensureDraft(subjectVersion: string): Promise<Review> {
  if (draft.value) return draft.value;
  const created = await createArtifactReview(props.id, {
    subject_kind: 'artifact',
    subject_key: props.artifactName,
    subject_version: subjectVersion,
  });
  replaceReview(created);
  return created;
}

async function saveOverallNote(): Promise<Review | null> {
  if (summaryHydrating.value) return draft.value;
  if (summarySaveTimer) {
    clearTimeout(summarySaveTimer);
    summarySaveTimer = null;
  }
  while (summarySavePromise) await summarySavePromise;
  if (!summaryDirty.value) return draft.value;

  const saving = (async () => {
    const summary = overallNote.value;
    let item = draft.value;
    if (!item) {
      if (!summary.trim()) {
        summaryDirty.value = false;
        return null;
      }
      item = await ensureDraft(String(props.rev));
    }
    if (item.summary === summary) {
      summaryDirty.value = false;
      return item;
    }
    summarySaving.value = true;
    trayError.value = '';
    try {
      const updated = await updateReview(item.id, {
        expected_revision: item.draft_revision,
        summary,
      });
      replaceReview(updated);
      if (overallNote.value === summary) summaryDirty.value = false;
      return updated;
    } catch (error) {
      trayError.value = mutationMessage(error);
      return null;
    } finally {
      summarySaving.value = false;
    }
  })();
  summarySavePromise = saving;
  try {
    return await saving;
  } finally {
    if (summarySavePromise === saving) summarySavePromise = null;
  }
}

async function createPendingComment() {
  const text = pendingDraft.value.trim();
  if (!pending.value || !text || savingComment.value) return;
  const pendingComment = pending.value;
  savingComment.value = true;
  composerError.value = '';
  try {
    const saved = await saveOverallNote();
    if (draft.value && !saved) return;
    const review = await ensureDraft(pendingComment.subjectVersion);
    const updated = await addReviewComment(review.id, {
      expected_revision: review.draft_revision,
      subject_version: pendingComment.subjectVersion,
      anchor_kind: 'text',
      anchor: pendingComment.anchor,
      body: text,
    });
    replaceReview(updated);
    activeId.value = updated.comments.at(-1)?.id ?? null;
    pending.value = null;
    pendingRange = null;
    pendingDraft.value = '';
    trayOpen.value = true;
    reviewNotice.value = 'Pending comment saved. Submit the review when your feedback is complete.';
    await nextTick();
    locateCycle();
  } catch (e) {
    composerError.value = mutationMessage(e);
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
  commentErrors.value[payload.commentId] = '';
  try {
    if (!(await saveOverallNote())) return;
    if (!draft.value) return;
    const updated = await updateReviewComment(draft.value.id, payload.commentId, {
      expected_revision: draft.value.draft_revision,
      body: payload.body,
    });
    replaceReview(updated);
    reviewNotice.value = 'Pending comment updated.';
  } catch (e) {
    commentErrors.value[payload.commentId] = mutationMessage(e);
  }
}

async function removeComment(commentId: number) {
  if (!draft.value) return;
  commentErrors.value[commentId] = '';
  try {
    if (!(await saveOverallNote())) return;
    if (!draft.value) return;
    const updated = await deleteReviewComment(
      draft.value.id,
      commentId,
      draft.value.draft_revision,
    );
    replaceReview(updated);
    if (activeId.value === commentId) activeId.value = null;
    if (reanchorCommentId.value === commentId) reanchorCommentId.value = null;
    reviewNotice.value = 'Pending comment deleted.';
    await nextTick();
    locateCycle();
  } catch (e) {
    commentErrors.value[commentId] = mutationMessage(e);
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
  trayError.value = '';
  try {
    const reviewId = draft.value.id;
    await discardReview(reviewId, draft.value.draft_revision);
    reviews.value = reviews.value.filter((review) => review.id !== reviewId);
    activeId.value = null;
    reanchorCommentId.value = null;
    overallNote.value = '';
    summaryDirty.value = false;
    acknowledgeOutdated.value = false;
    discardConfirm.value = false;
    trayOpen.value = false;
    reviewNotice.value = 'Draft review discarded.';
    await nextTick();
    locateCycle();
  } catch (e) {
    trayError.value = mutationMessage(e);
  }
}

async function submitDraft() {
  if (!draft.value || submitting.value) return;
  submitting.value = true;
  trayError.value = '';
  try {
    const saved = await saveOverallNote();
    if (!saved || !draft.value) return;
    const submitted = await submitReview(draft.value.id, {
      expected_revision: draft.value.draft_revision,
      acknowledge_outdated: acknowledgeOutdated.value,
    });
    replaceReview(submitted);
    activeId.value = null;
    reanchorCommentId.value = null;
    overallNote.value = '';
    summaryDirty.value = false;
    acknowledgeOutdated.value = false;
    reviewNotice.value =
      submitted.delivery_state === 'delivered'
        ? 'Review submitted to the conversation.'
        : 'Review submitted and queued for the conversation.';
    await nextTick();
    locateCycle();
  } catch (e) {
    trayError.value = mutationMessage(e);
  } finally {
    submitting.value = false;
  }
}

async function retryDelivery(review: Review) {
  try {
    const updated = await retryReviewDelivery(review.id);
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

function refreshPrivateDraft() {
  if (document.visibilityState === 'visible') void loadReviews();
}

onMounted(() => {
  activeScrollKey = scrollKey();
  void restoreScroll(activeScrollKey);
  document.addEventListener('selectionchange', onSelectionChange);
  document.addEventListener('mousedown', onDocMouseDown, true);
  document.addEventListener('visibilitychange', refreshPrivateDraft);
  window.addEventListener('focus', refreshPrivateDraft);
  void loadReviews();
});

onBeforeUnmount(() => {
  persistScroll();
  if (summarySaveTimer) clearTimeout(summarySaveTimer);
  if (!summaryHydrating.value && overallNote.value !== draft.value?.summary) {
    void saveOverallNote();
  }
  document.removeEventListener('selectionchange', onSelectionChange);
  document.removeEventListener('mousedown', onDocMouseDown, true);
  document.removeEventListener('visibilitychange', refreshPrivateDraft);
  window.removeEventListener('focus', refreshPrivateDraft);
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
      error: commentErrors.value[entry.comment.id] ?? '',
      onFocus: focusComment,
      onClose: closeComment,
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
          if (event.key === 'Escape') {
            event.preventDefault();
            cancelPendingComment();
            buttonEl.value?.focus();
          }
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
      composerError.value
        ? h(
            'p',
            {
              class: 'mt-1.5 rounded bg-block-soft px-2 py-1 text-2xs text-block',
              role: 'alert',
            },
            composerError.value,
          )
        : null,
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
            <span v-if="failedSubmissions.length" class="ml-1 text-block">
              · {{ failedSubmissions.length }} delivery failed
            </span>
          </template>
          <template v-else-if="failedSubmissions.length">
            Review delivery · {{ failedSubmissions.length }} failed
          </template>
          <template v-else-if="recentSubmitted">
            Review submitted · {{ recentSubmitted.delivery_state }}
          </template>
          <template v-else>Review artifact</template>
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
        <div
          v-if="failedSubmissions.length"
          class="mb-3 space-y-2"
          aria-label="Failed review deliveries"
        >
          <div
            v-for="item in failedSubmissions"
            :key="`failed-${item.id}`"
            class="rounded border border-block-line bg-block-soft p-2 text-xs"
            :data-testid="`failed-review-delivery-${item.id}`"
          >
            <p class="font-medium text-block">Review {{ item.id }} delivery failed</p>
            <p v-if="item.delivery_error" class="mt-1 text-block">{{ item.delivery_error }}</p>
            <button
              type="button"
              class="btn-primary mt-2 px-2 py-1 text-xs"
              @click="retryDelivery(item)"
            >
              Retry delivery
            </button>
          </div>
        </div>

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
            @blur="saveOverallNote"
          ></textarea>
          <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
            Saving overall note…
          </p>

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
            <p
              v-if="trayError"
              class="w-full rounded bg-block-soft px-2 py-1 text-2xs text-block"
              role="alert"
            >
              {{ trayError }}
            </p>
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
                summarySaving ||
                (!draft.comments.length && !overallNote.trim()) ||
                (draft.outdated && !acknowledgeOutdated)
              "
              @click="submitDraft"
            >
              {{ submitting ? 'Submitting…' : 'Submit review' }}
            </button>
          </div>
        </template>

        <template v-else>
          <div
            v-if="
              recentSubmitted &&
              !failedSubmissions.some((review) => review.id === recentSubmitted?.id)
            "
            class="mb-3 text-xs text-muted"
          >
            <p class="font-medium text-fg">Review {{ recentSubmitted.id }} submitted</p>
            <p class="mt-1">
              Delivery:
              <span class="text-accent">{{ recentSubmitted.delivery_state }}</span>
            </p>
          </div>
          <label class="block text-xs font-medium text-muted" for="review-new-overall-note">
            Start a new review with an overall note
          </label>
          <textarea
            id="review-new-overall-note"
            v-model="overallNote"
            rows="3"
            class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
            placeholder="Feedback that applies to the artifact as a whole…"
            data-testid="review-overall-note"
            @blur="saveOverallNote"
          ></textarea>
          <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
            Saving overall note…
          </p>
          <p
            v-if="trayError"
            class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block"
            role="alert"
          >
            {{ trayError }}
          </p>
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
