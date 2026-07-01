<script setup lang="ts">
import { nextTick, onMounted, onBeforeUnmount, ref, watch } from 'vue';
import type { Thread } from '../types';
import { listThreads, createThread, addComment, resolveThread } from '../api';
import {
  captureAnchor,
  locate,
  blockContaining,
  paintHighlights,
  clearHighlights,
  COMMENT_UI_ATTR,
  type TextAnchor,
} from '../discussion-anchor';
import CommentThread from './CommentThread.vue';

// The inline-comment controller for one artifact's rendered preview. It loads
// threads, locates each anchor in the live DOM (`discussion-anchor.ts`), paints
// the CSS Custom Highlight spans, and — Google-Wave style — renders each thread
// as a card *inside the document flow*, teleported into a placeholder inserted
// right after the block it annotates. No margin gutter, so the card uses the
// full text-column width (right for a half-screen rail) and the browser handles
// its position; there is no scroll/resize bookkeeping to keep.
//
// Mounted by ArtifactsPanel only in markdown preview mode — editing and
// non-markdown kinds have no comment layer.
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

const threads = ref<Thread[]>([]);
const activeId = ref<number | null>(null);

// New-thread composer state: the captured anchor, the live range it came from
// (used to place the composer under the right block), and its draft body.
const pending = ref<{ anchor: TextAnchor } | null>(null);
let pendingRange: Range | null = null;
const pendingDraft = ref('');

// The live located ranges backing the current paint — the click hit-test and
// focus-scroll read them.
const locatedThreads = ref<{ thread: Thread; range: Range }[]>([]);
// Open threads whose anchor failed to locate, plus server-flagged `orphaned`
// ones — read-only, shown in a footer disclosure at the end of the document.
const orphaned = ref<Thread[]>([]);
const showOrphaned = ref(false);

// Teleport targets: one placeholder per inline card, kept as direct children of
// the rendered body and marked so `buildTextMap` skips their contents. Rebuilt
// every locate cycle (a markdown re-render wipes them with the body's innerHTML).
const threadSlots = ref<{ tid: number; thread: Thread; el: HTMLElement }[]>([]);
const pendingSlot = ref<{ el: HTMLElement } | null>(null);
const orphanSlot = ref<{ el: HTMLElement } | null>(null);

// The floating "💬 Comment" button shown after a text selection inside the body.
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

// --- locate + paint + place -----------------------------------------------

function makeSlot(): HTMLElement {
  const el = document.createElement('div');
  el.setAttribute(COMMENT_UI_ATTR, '');
  return el;
}

// Remove the placeholders we inserted last cycle. Their teleported content is
// detached with them, but Vue holds the vnodes and re-homes each card into its
// fresh placeholder below (keyed by thread id), so drafts and focus survive.
function clearSlots(root: HTMLElement) {
  for (const child of Array.from(root.children)) {
    if (child instanceof HTMLElement && child.hasAttribute(COMMENT_UI_ATTR)) child.remove();
  }
}

function runLocateCycle() {
  const root = props.bodyEl;
  if (!root) {
    locatedThreads.value = [];
    threadSlots.value = [];
    pendingSlot.value = null;
    orphanSlot.value = null;
    orphaned.value = threads.value.filter((t) => t.status === 'orphaned');
    clearHighlights();
    return;
  }
  clearSlots(root);

  // Locate every open thread against the clean DOM (placeholders removed;
  // `buildTextMap` also excludes them, so a leftover never pollutes the search).
  const open = threads.value.filter((t) => t.status === 'open');
  const located: { thread: Thread; range: Range }[] = [];
  const unlocated: Thread[] = [];
  for (const thread of open) {
    const r = locate(root, thread.anchor);
    if (r) located.push({ thread, range: r });
    else unlocated.push(thread);
  }
  locatedThreads.value = located;

  const activeRange = located.find((x) => x.thread.id === activeId.value)?.range ?? null;
  paintHighlights(
    located.map((x) => x.range),
    activeRange,
  );

  // Place each card just after the block it annotates, in document order. When
  // several land on the same block they stack in that order beneath it.
  located.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));
  const lastAfter = new Map<HTMLElement, Node>();
  const insertAfterBlock = (block: HTMLElement | null, ph: HTMLElement) => {
    const after = block ? (lastAfter.get(block) ?? block) : root.lastChild;
    root.insertBefore(ph, after ? after.nextSibling : null);
    if (block) lastAfter.set(block, ph);
  };

  const slots: { tid: number; thread: Thread; el: HTMLElement }[] = [];
  for (const { thread, range } of located) {
    const block = blockContaining(root, range.endContainer) ?? blockContaining(root, range.startContainer);
    const ph = makeSlot();
    insertAfterBlock(block, ph);
    slots.push({ tid: thread.id, thread, el: ph });
  }
  threadSlots.value = slots;

  // The new-thread composer sits under the block its selection ended in. If a
  // re-render invalidated the range mid-compose (rare), fall back to the end so
  // the draft is never dropped.
  if (pending.value && pendingRange && root.contains(pendingRange.endContainer)) {
    const block = blockContaining(root, pendingRange.endContainer);
    const ph = makeSlot();
    insertAfterBlock(block, ph);
    pendingSlot.value = { el: ph };
  } else if (pending.value) {
    const ph = makeSlot();
    root.appendChild(ph);
    pendingSlot.value = { el: ph };
  } else {
    pendingSlot.value = null;
  }

  // Unanchored footer at the very end of the document.
  orphaned.value = [...unlocated, ...threads.value.filter((t) => t.status === 'orphaned')];
  if (orphaned.value.length) {
    const ph = makeSlot();
    root.appendChild(ph);
    orphanSlot.value = { el: ph };
  } else {
    orphanSlot.value = null;
  }
}

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
  // A selection inside an existing card (reply/quote text) is not a new anchor.
  const start = range.startContainer;
  if ((start instanceof Element ? start : start.parentElement)?.closest(`[${COMMENT_UI_ATTR}]`)) {
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
  pending.value = { anchor: selectionButton.value.anchor };
  pendingDraft.value = '';
  selectionButton.value = null;
  sel?.removeAllRanges();
  runLocateCycle();
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

// Clicking a painted highlight expands its thread. There's no cheap way to know
// which highlight a click landed on directly (the CSS Custom Highlight API
// paints without wrapper elements), so this falls back to the browser's
// caret-from-point APIs and checks which located Range contains that caret.
// Best-effort: unsupported browsers, or a click that misses the caret APIs,
// simply do nothing — the inline chips remain the reliable path.
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

// --- events ------------------------------------------------------------------

function focusThread(tid: number) {
  activeId.value = tid;
  runLocateCycle();
  const entry = locatedThreads.value.find((x) => x.thread.id === tid);
  const scroller = scrollerEl();
  if (!entry || !scroller) return;
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
    // The reply landed — the thread's comment count grew, which is the card's
    // cue to clear its draft. A failure leaves the count (and draft) untouched
    // so the text can be resubmitted.
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

async function onCreate() {
  const text = pendingDraft.value.trim();
  if (!pending.value || !text) return;
  try {
    const thread = await createThread(props.sessionId, props.artifactName, {
      base_rev: props.rev,
      anchor: pending.value.anchor,
      body: text,
    });
    threads.value = [...threads.value, thread];
    activeId.value = thread.id;
    // Only close the composer on success; a failure below keeps it open with
    // the draft intact for a retry.
    pending.value = null;
    pendingRange = null;
    pendingDraft.value = '';
    await nextTick();
    runLocateCycle();
  } catch (e) {
    console.warn('failed to create comment thread', e);
  }
}

function onCancel() {
  pending.value = null;
  pendingRange = null;
  pendingDraft.value = '';
  runLocateCycle();
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

watch(
  () => props.bodyEl,
  (el, oldEl) => {
    if (oldEl) detachBody(oldEl);
    if (el) attachBody(el);
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
    pendingDraft.value = '';
    selectionButton.value = null;
    showOrphaned.value = false;
    clearHighlights();
    loadThreads();
  },
);

onMounted(() => {
  loadThreads();
  document.addEventListener('selectionchange', onSelectionChange);
  document.addEventListener('mousedown', onDocMouseDown, true);
});

onBeforeUnmount(() => {
  if (props.bodyEl) {
    detachBody(props.bodyEl);
    clearSlots(props.bodyEl);
  }
  document.removeEventListener('selectionchange', onSelectionChange);
  document.removeEventListener('mousedown', onDocMouseDown, true);
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
  <!-- A thin overlay that hosts only the transient selection button; the thread
       cards live inline in the document, teleported into the placeholders. -->
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
  </div>

  <!-- Inline thread cards, in the document flow under the block they annotate. -->
  <Teleport v-for="slot in threadSlots" :key="slot.tid" :to="slot.el">
    <CommentThread
      :thread="slot.thread"
      :active="slot.tid === activeId"
      @focus="focusThread"
      @reply="onReply"
      @resolve="onResolve"
    />
  </Teleport>

  <!-- New-thread composer, inline under the selected block. -->
  <Teleport v-if="pendingSlot" :to="pendingSlot.el">
    <div
      class="my-2 rounded border border-accent bg-subtle/40 p-2 text-xs ring-1 ring-accent"
      data-testid="comment-pending"
      @click.stop
    >
      <textarea
        v-model="pendingDraft"
        rows="2"
        placeholder="Comment…"
        class="w-full resize-none rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
        @click.stop
        @mousedown.stop
      ></textarea>
      <div class="mt-1.5 flex items-center gap-1.5">
        <button type="button" class="btn-primary px-2 py-1 text-2xs" @click.stop="onCreate">
          Comment
        </button>
        <button type="button" class="btn-secondary px-2 py-1 text-2xs" @click.stop="onCancel">
          Cancel
        </button>
      </div>
    </div>
  </Teleport>

  <!-- Unanchored footer — read-only threads whose quote no longer locates. -->
  <Teleport v-if="orphanSlot" :to="orphanSlot.el">
    <div class="my-3 border-t border-line pt-2" data-testid="comment-orphaned" @click.stop>
      <button
        type="button"
        class="pill flex w-full items-center justify-between px-2 py-1 text-2xs"
        @click.stop="showOrphaned = !showOrphaned"
      >
        <span>Unanchored comments ({{ orphaned.length }})</span>
        <span>{{ showOrphaned ? '▾' : '▸' }}</span>
      </button>
      <div v-if="showOrphaned" class="mt-1.5 space-y-2">
        <div
          v-for="t in orphaned"
          :key="t.id"
          class="rounded border border-line bg-subtle/40 p-2 text-xs"
        >
          <div class="truncate italic text-faint">&ldquo;{{ t.anchor.quote }}&rdquo;</div>
          <div class="mt-0.5 truncate text-fg">
            {{ t.comments[t.comments.length - 1]?.body }}
          </div>
          <button
            type="button"
            class="btn-secondary mt-1 px-2 py-0.5 text-2xs"
            @click.stop="onResolve(t.id)"
          >
            Resolve
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
