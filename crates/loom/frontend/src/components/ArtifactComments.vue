<script setup lang="ts">
import { computed, nextTick, onMounted, onBeforeUnmount, ref, watch } from 'vue';
import type { Thread } from '../types';
import { listThreads, createThread, addComment, resolveThread } from '../api';
import {
  captureAnchor,
  locate,
  paintHighlights,
  clearHighlights,
  type TextAnchor,
} from '../discussion-anchor';
import CommentRail from './CommentRail.vue';

// The margin-comment controller for one artifact's rendered preview: loads
// threads, locates their anchors in the live DOM (`discussion-anchor.ts`),
// paints the CSS Custom Highlight spans, and positions a CommentRail card per
// located thread. Mounted by ArtifactsPanel only in markdown preview mode —
// editing and non-markdown kinds have no comment layer.
const props = defineProps<{
  sessionId: string;
  artifactName: string;
  /** The artifact's current latest revision — stamped on a new thread's anchor. */
  rev: number;
  /** The rendered <article>, from MarkdownView's `defineExpose({ body })`. Null
   *  until the first render lands. */
  bodyEl: HTMLElement | null;
  /** Bumped by the parent on every MarkdownView `@rendered` — the cue to
   *  relocate every anchor against the fresh DOM. */
  renderNonce: number;
}>();

// Collapsed card height and the vertical space reserved for the one expanded
// (active) card, used by the downward de-overlap pass below. Approximate —
// there's no DOM measurement pass, just a reasonable reserve.
const COLLAPSED_H = 46;
const EXPANDED_GAP = 260;

const threads = ref<Thread[]>([]);
const activeId = ref<number | null>(null);
const pending = ref<{ anchor: TextAnchor; top: number } | null>(null);
let pendingRange: Range | null = null;

// Raw (pre-de-overlap) card top per thread id, recomputed on scroll/resize.
const positions = ref<Record<number, number>>({});
// The live located ranges backing the current paint + position pass.
const locatedThreads = ref<{ thread: Thread; range: Range }[]>([]);
// Open threads whose anchor failed to locate, plus server-flagged `orphaned`
// ones — read-only, listed in the rail's footer.
const orphaned = ref<Thread[]>([]);

// The floating "💬 Comment" button shown after a text selection inside the
// rendered body.
const selectionButton = ref<{ anchor: TextAnchor; top: number; left: number } | null>(null);
const buttonEl = ref<HTMLElement | null>(null);

function scrollerEl(): HTMLElement | null {
  return props.bodyEl?.parentElement ?? null;
}

// --- load ---------------------------------------------------------------

async function loadThreads() {
  try {
    threads.value = await listThreads(props.sessionId, props.artifactName);
  } catch (e) {
    // A transient failure shouldn't wipe already-loaded threads out from under
    // the reader; keep what we have and log for diagnosis. The next render /
    // SSE refetch recovers.
    console.warn('failed to load comment threads', e);
  }
  await nextTick();
  runLocateCycle();
}

// --- locate + paint -------------------------------------------------------

function runLocateCycle() {
  const root = props.bodyEl;
  if (!root) {
    locatedThreads.value = [];
    orphaned.value = threads.value.filter((t) => t.status === 'orphaned');
    clearHighlights();
    return;
  }
  const open = threads.value.filter((t) => t.status === 'open');
  const located: { thread: Thread; range: Range }[] = [];
  const unlocated: Thread[] = [];
  for (const thread of open) {
    const r = locate(root, thread.anchor);
    if (r) located.push({ thread, range: r });
    else unlocated.push(thread);
  }
  locatedThreads.value = located;
  orphaned.value = [...unlocated, ...threads.value.filter((t) => t.status === 'orphaned')];
  const activeRange = located.find((x) => x.thread.id === activeId.value)?.range ?? null;
  paintHighlights(
    located.map((x) => x.range),
    activeRange,
  );
  recomputePositions();
}

// --- position (cheap: no re-locate, just re-read live rect geometry) ------

function recomputePositions() {
  const scroller = scrollerEl();
  if (!scroller) return;
  const scrollerRect = scroller.getBoundingClientRect();
  const next: Record<number, number> = {};
  for (const { thread, range } of locatedThreads.value) {
    next[thread.id] = range.getBoundingClientRect().top - scrollerRect.top;
  }
  positions.value = next;
  if (pending.value && pendingRange) {
    pending.value = { ...pending.value, top: pendingRange.getBoundingClientRect().top - scrollerRect.top };
  }
}

let rafId: number | null = null;
function scheduleRecompute() {
  if (rafId != null) return;
  rafId = requestAnimationFrame(() => {
    rafId = null;
    recomputePositions();
  });
}

// --- cards (presentational shape for CommentRail) --------------------------

const cards = computed(() => {
  const raw = locatedThreads.value
    .filter(({ thread }) => thread.status === 'open')
    .map(({ thread }) => ({ thread, top: positions.value[thread.id] ?? 0 }))
    .sort((a, b) => a.top - b.top);
  let cursor = -Infinity;
  const out: Array<{ thread: Thread; top: number; active: boolean; located: boolean }> = [];
  for (const { thread, top: rawTop } of raw) {
    const active = thread.id === activeId.value;
    const top = Math.max(rawTop, cursor);
    cursor = top + (active ? EXPANDED_GAP : COLLAPSED_H);
    out.push({ thread, top, active, located: true });
  }
  return out;
});

// --- selection -> new comment -----------------------------------------------

function onMouseUp() {
  const root = props.bodyEl;
  const sel = window.getSelection();
  if (!root || !sel || sel.rangeCount === 0) {
    selectionButton.value = null;
    return;
  }
  const range = sel.getRangeAt(0);
  if (range.collapsed || !root.contains(range.startContainer) || !root.contains(range.endContainer)) {
    selectionButton.value = null;
    return;
  }
  const anchor = captureAnchor(root, range);
  if (!anchor) {
    selectionButton.value = null;
    return;
  }
  const scroller = scrollerEl();
  if (!scroller) return;
  const scrollerRect = scroller.getBoundingClientRect();
  const rect = range.getBoundingClientRect();
  selectionButton.value = {
    anchor,
    top: rect.bottom - scrollerRect.top + 4,
    left: Math.max(0, rect.right - scrollerRect.left - 96),
  };
}

function openComposer() {
  if (!selectionButton.value) return;
  const sel = window.getSelection();
  pendingRange = sel && sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
  pending.value = { anchor: selectionButton.value.anchor, top: selectionButton.value.top };
  selectionButton.value = null;
  sel?.removeAllRanges();
}

function onSelectionChange() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed) selectionButton.value = null;
}

function onDocMouseDown(e: MouseEvent) {
  const target = e.target as Node;
  if (buttonEl.value && buttonEl.value.contains(target)) return;
  selectionButton.value = null;
}

// --- click-to-focus (best-effort hit test) ----------------------------------

// Clicking a painted highlight focuses its thread's card. There's no cheap way
// to know which highlight a click landed on directly (the CSS Custom Highlight
// API paints without wrapper elements), so this falls back to the browser's
// caret-from-point APIs and checks which located Range contains that caret.
// Best-effort: unsupported browsers, or a click that misses the caret APIs,
// simply do nothing — the rail list and card clicks remain the reliable path.
function onBodyClick(e: MouseEvent) {
  const sel = window.getSelection();
  if (sel && !sel.isCollapsed) return; // a drag-selection, not a plain click
  const doc = document as Document & {
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  let node: Node | null = null;
  let offset = 0;
  if (doc.caretRangeFromPoint) {
    const r = doc.caretRangeFromPoint(e.clientX, e.clientY);
    if (r) {
      node = r.startContainer;
      offset = r.startOffset;
    }
  } else if (doc.caretPositionFromPoint) {
    const p = doc.caretPositionFromPoint(e.clientX, e.clientY);
    if (p) {
      node = p.offsetNode;
      offset = p.offset;
    }
  }
  if (!node) return;
  const hit = locatedThreads.value.find(({ range }) => {
    try {
      return range.isPointInRange(node as Node, offset);
    } catch {
      return false;
    }
  });
  if (hit) focusThread(hit.thread.id);
}

// --- rail events -------------------------------------------------------------

function focusThread(tid: number) {
  activeId.value = tid;
  runLocateCycle();
  const entry = locatedThreads.value.find((x) => x.thread.id === tid);
  if (!entry) return;
  const scroller = scrollerEl();
  if (!scroller) return;
  const scrollerRect = scroller.getBoundingClientRect();
  const rect = entry.range.getBoundingClientRect();
  if (rect.top >= scrollerRect.top && rect.bottom <= scrollerRect.bottom) return;
  type ScrollableRange = Range & { scrollIntoView?: (opts?: ScrollIntoViewOptions) => void };
  const r = entry.range as ScrollableRange;
  if (typeof r.scrollIntoView === 'function') {
    r.scrollIntoView({ block: 'center' });
  } else {
    const el =
      entry.range.startContainer.nodeType === Node.TEXT_NODE
        ? entry.range.startContainer.parentElement
        : (entry.range.startContainer as Element);
    el?.scrollIntoView({ block: 'center' });
  }
}

async function onReply(payload: { tid: number; body: string }) {
  const body = payload.body.trim();
  if (!body) return;
  try {
    const comment = await addComment(props.sessionId, props.artifactName, payload.tid, { body });
    const t = threads.value.find((t) => t.id === payload.tid);
    if (t) t.comments = [...t.comments, comment];
    // The reply landed — the thread's comment count grew, which is CommentRail's
    // cue to clear that card's draft. A failure leaves the count (and the draft)
    // untouched so the text can be resubmitted.
  } catch (e) {
    console.warn('failed to post reply', e);
  }
}

async function onResolve(tid: number) {
  try {
    const updated = await resolveThread(props.sessionId, props.artifactName, tid);
    const idx = threads.value.findIndex((t) => t.id === tid);
    if (idx !== -1) threads.value[idx] = updated;
    if (activeId.value === tid) activeId.value = null;
    runLocateCycle();
  } catch {
    /* leave it open; the user can retry */
  }
}

async function onCreate(body: string) {
  const text = body.trim();
  if (!pending.value || !text) return;
  try {
    const thread = await createThread(props.sessionId, props.artifactName, {
      base_rev: props.rev,
      anchor: pending.value.anchor,
      body: text,
    });
    threads.value = [...threads.value, thread];
    activeId.value = thread.id;
    // Only close the composer on success — clearing `pending` unmounts it,
    // which is CommentRail's cue to drop the draft. A failure below leaves the
    // composer open with its text intact for a retry.
    pending.value = null;
    pendingRange = null;
    await nextTick();
    runLocateCycle();
  } catch (e) {
    console.warn('failed to create comment thread', e);
  }
}

function onCancel() {
  pending.value = null;
  pendingRange = null;
}

// --- lifecycle --------------------------------------------------------------

function attachBody(root: HTMLElement) {
  root.addEventListener('mouseup', onMouseUp);
  root.addEventListener('click', onBodyClick);
}
function detachBody(root: HTMLElement) {
  root.removeEventListener('mouseup', onMouseUp);
  root.removeEventListener('click', onBodyClick);
}

let currentScroller: HTMLElement | null = null;
let resizeObserver: ResizeObserver | null = null;
function attachScroller(scroller: HTMLElement) {
  scroller.addEventListener('scroll', scheduleRecompute, { passive: true });
  resizeObserver = new ResizeObserver(scheduleRecompute);
  resizeObserver.observe(scroller);
  currentScroller = scroller;
}
function detachScroller() {
  if (!currentScroller) return;
  currentScroller.removeEventListener('scroll', scheduleRecompute);
  resizeObserver?.disconnect();
  resizeObserver = null;
  currentScroller = null;
}

watch(
  () => props.bodyEl,
  (el, oldEl) => {
    if (oldEl) detachBody(oldEl);
    detachScroller();
    if (el) {
      attachBody(el);
      const scroller = el.parentElement;
      if (scroller) attachScroller(scroller);
    }
    runLocateCycle();
  },
  { immediate: true },
);

watch(
  () => props.renderNonce,
  () => runLocateCycle(),
);

watch(
  () => props.artifactName,
  () => {
    threads.value = [];
    activeId.value = null;
    pending.value = null;
    pendingRange = null;
    selectionButton.value = null;
    clearHighlights();
    loadThreads();
  },
);

onMounted(() => {
  loadThreads();
  window.addEventListener('resize', scheduleRecompute);
  document.addEventListener('selectionchange', onSelectionChange);
  document.addEventListener('mousedown', onDocMouseDown, true);
});

onBeforeUnmount(() => {
  if (props.bodyEl) detachBody(props.bodyEl);
  detachScroller();
  window.removeEventListener('resize', scheduleRecompute);
  document.removeEventListener('selectionchange', onSelectionChange);
  document.removeEventListener('mousedown', onDocMouseDown, true);
  if (rafId != null) cancelAnimationFrame(rafId);
  clearHighlights();
});

// --- SSE forwarding (ArtifactsPanel owns the one EventSource) --------------

async function onCommentEvent(kind: string, data: { artifact?: string; thread?: number }): Promise<void> {
  if (data.artifact && data.artifact !== props.artifactName) return;
  if (kind !== 'comment_added' && kind !== 'comment_resolved') return;
  await loadThreads();
  if (activeId.value != null && !threads.value.some((t) => t.id === activeId.value)) {
    activeId.value = null;
  }
}

defineExpose({ onCommentEvent });
</script>

<template>
  <div class="pointer-events-none absolute inset-0 overflow-hidden" data-testid="artifact-comments">
    <button
      v-if="selectionButton"
      ref="buttonEl"
      type="button"
      class="btn-primary pointer-events-auto absolute z-20 gap-1 px-2 py-1 text-xs shadow-sm"
      data-testid="comment-select-button"
      :style="{ top: selectionButton.top + 'px', left: selectionButton.left + 'px' }"
      @mousedown.prevent
      @click="openComposer"
    >
      💬 Comment
    </button>

    <CommentRail
      :cards="cards"
      :pending="pending"
      :orphaned="orphaned"
      @reply="onReply"
      @resolve="onResolve"
      @create="onCreate"
      @cancel="onCancel"
      @focus="focusThread"
    />
  </div>
</template>
