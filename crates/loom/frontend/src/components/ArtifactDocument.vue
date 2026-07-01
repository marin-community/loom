<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount, nextTick, h, isVNode, type VNode } from 'vue';
import { useRouter } from 'vue-router';
import { renderTokens } from '../markdown-render';
import { useMarkdownDoc, routeDocLink } from '../lib/markdownDoc';
import type { IssueRefStatus, Thread } from '../types';
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

// The collaborative markdown surface: one component that renders the artifact as
// a real Vue tree (the same token→vnode renderer MarkdownView uses) AND owns the
// inline comment layer, Google-Wave style — each thread's card renders *in the
// document flow* as a plain vnode right after the block its quote anchors to.
//
// This replaces the old MarkdownView+ArtifactComments pair, whose comment layer
// was hand-wired imperative DOM (a mouseup listener bolted onto an innerHTML
// blob, cards teleported into spliced-in placeholders). That wiring silently
// died when the panel went through a keep-alive / v-show round-trip. Here the
// <article> is owned by this component and never swapped, the mouseup handler is
// a Vue template binding (so it can't detach), and the cards are real vnodes
// keyed by block — so a warm panel keeps working.
//
// Anchoring is unchanged and correct: stand-off W3C text-quote selectors located
// in the rendered DOM (`discussion-anchor.ts`) and painted via the CSS Custom
// Highlight API — no wrapper elements, no source-offset map.
const props = defineProps<{
  /** Session id — for the markdown image/link context and the comment API. */
  id: string;
  /** Pseudo-path (e.g. `goal.md`) — anchors relative image resolution. */
  path: string;
  /** Raw markdown source of the artifact revision on screen. */
  source: string;
  /** Live `#N` issue status map for the smartdoc projection. */
  refs?: Record<string, IssueRefStatus>;
  /** The artifact name — the comment threads' owning document. */
  artifactName: string;
  /** The artifact's current latest revision — stamped on a new thread's anchor. */
  rev: number;
}>();

const router = useRouter();

// --- markdown body ----------------------------------------------------------

const containerEl = ref<HTMLElement | null>(null);
const scrollerEl = ref<HTMLElement | null>(null);

// The shared markdown build pipeline: parse → reactive `tokens`/`ctx`, with the
// out-of-order guard and the source watch. After each build lands (the fresh
// tree painted) we re-run the comment locate pass against the new DOM.
const { body, error, tokens, ctx } = useMarkdownDoc(props, () => locateCycle());

// --- comment state ----------------------------------------------------------

const threads = ref<Thread[]>([]);
const activeId = ref<number | null>(null);

// Located open threads grouped by the top-level block index they sit under, plus
// the live ranges backing the paint (read by the click hit-test and focus scroll)
// and the open threads whose quote no longer locates (the unanchored footer).
const cardsByBlock = ref<Map<number, Thread[]>>(new Map());
const locatedThreads = ref<{ thread: Thread; range: Range }[]>([]);
const orphaned = ref<Thread[]>([]);
const showOrphaned = ref(false);

// New-thread composer: the captured anchor, its live range (to place the card
// under the right block), the block it lands under, and the draft body.
const pending = ref<{ anchor: TextAnchor } | null>(null);
let pendingRange: Range | null = null;
const pendingBlock = ref<number>(-1);
const pendingDraft = ref('');

// The floating "💬 Comment" button after a selection.
const selectionButton = ref<{ anchor: TextAnchor; top: number; left: number } | null>(null);
const buttonEl = ref<HTMLElement | null>(null);

async function loadThreads() {
  try {
    threads.value = await listThreads(props.id, props.artifactName);
  } catch (e) {
    // A transient failure shouldn't wipe already-loaded threads; keep what we
    // have and recover on the next render / SSE refetch.
    console.warn('failed to load comment threads', e);
  }
  await nextTick();
  locateCycle();
}

// --- locate + paint + group -------------------------------------------------

// Locate every open thread against the rendered DOM, paint the highlights, and
// group each into the block it annotates so the render function can interleave
// its card. Placeholders and cards are marked COMMENT_UI_ATTR, which
// `buildTextMap` skips — so a card's own echo of a quote never pollutes a search.
function locateCycle() {
  const root = body.value;
  if (!root) {
    cardsByBlock.value = new Map();
    locatedThreads.value = [];
    orphaned.value = threads.value.filter((t) => t.status === 'orphaned');
    clearHighlights();
    return;
  }
  const open = threads.value.filter((t) => t.status === 'open');
  const located: { thread: Thread; range: Range; block: number }[] = [];
  const unlocated: Thread[] = [];
  for (const thread of open) {
    const r = locate(root, thread.anchor);
    if (!r) {
      unlocated.push(thread);
      continue;
    }
    const el = blockContaining(root, r.endContainer) ?? blockContaining(root, r.startContainer);
    const attr = el?.getAttribute('data-block');
    located.push({ thread, range: r, block: attr != null ? Number(attr) : -1 });
  }
  locatedThreads.value = located.map((x) => ({ thread: x.thread, range: x.range }));

  const activeRange = located.find((x) => x.thread.id === activeId.value)?.range ?? null;
  paintHighlights(
    located.map((x) => x.range),
    activeRange,
  );

  // Document order within a block: several cards on one block stack in reading
  // order beneath it.
  located.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));
  const byBlock = new Map<number, Thread[]>();
  for (const { thread, block } of located) {
    const arr = byBlock.get(block);
    if (arr) arr.push(thread);
    else byBlock.set(block, [thread]);
  }
  cardsByBlock.value = byBlock;

  orphaned.value = [...unlocated, ...threads.value.filter((t) => t.status === 'orphaned')];

  // The composer sits under the block its selection ended in (else the very end).
  if (pending.value && pendingRange && root.contains(pendingRange.endContainer)) {
    const el = blockContaining(root, pendingRange.endContainer);
    const attr = el?.getAttribute('data-block');
    pendingBlock.value = attr != null ? Number(attr) : -1;
  } else if (!pending.value) {
    pendingBlock.value = -1;
  }
}

// Repaint / re-place when the focused thread changes.
watch(activeId, () => locateCycle());

// --- selection → new comment ------------------------------------------------

function onMouseUp() {
  const root = body.value;
  const sel = window.getSelection();
  if (!root || !sel || sel.rangeCount === 0) {
    selectionButton.value = null;
    return;
  }
  const range = sel.getRangeAt(0);
  if (
    range.collapsed ||
    !root.contains(range.startContainer) ||
    !root.contains(range.endContainer)
  ) {
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
  const container = containerEl.value;
  if (!container) return;
  const cRect = container.getBoundingClientRect();
  const rect = range.getBoundingClientRect();
  selectionButton.value = {
    anchor,
    top: rect.bottom - cRect.top + 4,
    left: Math.max(0, rect.right - cRect.left - 96),
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
  locateCycle();
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

// --- click: links + click-to-focus ------------------------------------------

// A click on a rendered link routes it (smartdoc SPA links / in-page scroll);
// otherwise a plain click on a painted span expands its thread. The CSS Custom
// Highlight API paints without wrapper elements, so there's no element to click —
// this falls back to the browser's caret-from-point APIs and checks which located
// Range contains that caret. Best-effort: an unsupported browser simply does
// nothing (the inline chips remain the reliable path).
function onArticleClick(e: MouseEvent) {
  // A click on any in-document link is routed by the shared helper (or left to
  // the browser for external links); only a plain, non-link click falls through
  // to the caret hit-test that focuses the thread sitting under the caret.
  if (routeDocLink(e, router, body.value)) return;
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

// --- thread events ----------------------------------------------------------

function focusThread(tid: number) {
  activeId.value = tid;
  const entry = locatedThreads.value.find((x) => x.thread.id === tid);
  const scroller = scrollerEl.value;
  if (!entry || !scroller) return;
  const sRect = scroller.getBoundingClientRect();
  const rect = entry.range.getBoundingClientRect();
  if (rect.top >= sRect.top && rect.bottom <= sRect.bottom) return;
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
    const comment = await addComment(props.id, props.artifactName, payload.tid, { body });
    const t = threads.value.find((t) => t.id === payload.tid);
    // The comment count growing is CommentThread's cue to clear its draft; a
    // failure leaves the count (and draft) untouched for a retry.
    if (t) t.comments = [...t.comments, comment];
  } catch (e) {
    console.warn('failed to post reply', e);
  }
}

async function onResolve(tid: number) {
  try {
    const updated = await resolveThread(props.id, props.artifactName, tid);
    const idx = threads.value.findIndex((t) => t.id === tid);
    if (idx !== -1) {
      const copy = [...threads.value];
      copy[idx] = updated;
      threads.value = copy;
    }
    if (activeId.value === tid) activeId.value = null;
    await nextTick();
    locateCycle();
  } catch {
    /* leave it open; the user can retry */
  }
}

async function onCreate() {
  const text = pendingDraft.value.trim();
  if (!pending.value || !text) return;
  try {
    const thread = await createThread(props.id, props.artifactName, {
      base_rev: props.rev,
      anchor: pending.value.anchor,
      body: text,
    });
    threads.value = [...threads.value, thread];
    activeId.value = thread.id;
    // Only close the composer on success; a failure keeps it open with the draft.
    pending.value = null;
    pendingRange = null;
    pendingDraft.value = '';
    await nextTick();
    locateCycle();
  } catch (e) {
    console.warn('failed to create comment thread', e);
  }
}

function onCancel() {
  pending.value = null;
  pendingRange = null;
  pendingDraft.value = '';
  locateCycle();
}

// --- SSE forwarding (ArtifactsPanel owns the one EventSource) ----------------

async function onCommentEvent(
  kind: string,
  data: { artifact?: string; thread?: number },
): Promise<void> {
  if (data.artifact && data.artifact !== props.artifactName) return;
  if (kind !== 'comment_added' && kind !== 'comment_resolved') return;
  await loadThreads();
  if (activeId.value != null && !threads.value.some((t) => t.id === activeId.value)) {
    activeId.value = null;
  }
}

defineExpose({ onCommentEvent });

// --- lifecycle --------------------------------------------------------------

onMounted(() => {
  document.addEventListener('selectionchange', onSelectionChange);
  document.addEventListener('mousedown', onDocMouseDown, true);
  // `useMarkdownDoc` already runs the initial build on mount; we just load the
  // threads that overlay it.
  loadThreads();
});

onBeforeUnmount(() => {
  document.removeEventListener('selectionchange', onSelectionChange);
  document.removeEventListener('mousedown', onDocMouseDown, true);
  clearHighlights();
});

// --- render -----------------------------------------------------------------

function withStop(fn: () => void) {
  return (e: Event) => {
    e.stopPropagation();
    fn();
  };
}

function renderCard(t: Thread): VNode {
  return h(
    'div',
    { [COMMENT_UI_ATTR]: '', key: `t${t.id}` },
    h(CommentThread, {
      thread: t,
      active: t.id === activeId.value,
      onFocus: focusThread,
      onReply: onReply,
      onResolve: onResolve,
    }),
  );
}

function renderComposer(): VNode {
  return h(
    'div',
    {
      key: 'pending',
      [COMMENT_UI_ATTR]: '',
      class: 'my-2 rounded border border-accent bg-subtle/40 p-2 text-xs ring-1 ring-accent',
      'data-testid': 'comment-pending',
      onClick: (e: Event) => e.stopPropagation(),
    },
    [
      h('textarea', {
        value: pendingDraft.value,
        rows: 2,
        placeholder: 'Comment…',
        class:
          'w-full resize-none rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent',
        onInput: (e: Event) => {
          pendingDraft.value = (e.target as HTMLTextAreaElement).value;
        },
        onClick: (e: Event) => e.stopPropagation(),
        onMousedown: (e: Event) => e.stopPropagation(),
      }),
      h('div', { class: 'mt-1.5 flex items-center gap-1.5' }, [
        h(
          'button',
          { type: 'button', class: 'btn-primary px-2 py-1 text-2xs', onClick: withStop(onCreate) },
          'Comment',
        ),
        h(
          'button',
          { type: 'button', class: 'btn-secondary px-2 py-1 text-2xs', onClick: withStop(onCancel) },
          'Cancel',
        ),
      ]),
    ],
  );
}

function renderOrphaned(): VNode {
  return h(
    'div',
    {
      key: 'orphaned',
      [COMMENT_UI_ATTR]: '',
      class: 'my-3 border-t border-line pt-2',
      'data-testid': 'comment-orphaned',
      onClick: (e: Event) => e.stopPropagation(),
    },
    [
      h(
        'button',
        {
          type: 'button',
          class: 'pill flex w-full items-center justify-between px-2 py-1 text-2xs',
          onClick: withStop(() => {
            showOrphaned.value = !showOrphaned.value;
          }),
        },
        [
          h('span', `Unanchored comments (${orphaned.value.length})`),
          h('span', showOrphaned.value ? '▾' : '▸'),
        ],
      ),
      showOrphaned.value
        ? h(
            'div',
            { class: 'mt-1.5 space-y-2' },
            orphaned.value.map((t) =>
              h('div', { key: t.id, class: 'rounded border border-line bg-subtle/40 p-2 text-xs' }, [
                h('div', { class: 'truncate italic text-faint' }, `“${t.anchor.quote}”`),
                h('div', { class: 'mt-0.5 truncate text-fg' }, t.comments[t.comments.length - 1]?.body),
                h(
                  'button',
                  {
                    type: 'button',
                    class: 'btn-secondary mt-1 px-2 py-0.5 text-2xs',
                    onClick: withStop(() => onResolve(t.id)),
                  },
                  'Resolve',
                ),
              ]),
            ),
          )
        : null,
    ],
  );
}

// The document body: markdown blocks, each followed by the comment cards (and,
// where the selection landed, the composer) that annotate it. Recreated every
// render — fresh vnodes, keyed so Vue reuses the block/card DOM (and its child
// component state) across renders and locate cycles.
const DocBody = () => {
  const c = ctx.value;
  if (!c) return null;
  const blocks = renderTokens(tokens.value, c);
  const out: (VNode | string)[] = [];
  const placed = new Set<number>();
  blocks.forEach((blk, i) => {
    out.push(blk);
    for (const t of cardsByBlock.value.get(i) ?? []) out.push(renderCard(t));
    if (pending.value && pendingBlock.value === i) out.push(renderComposer());
    placed.add(i);
  });
  // Cards whose block index fell outside the block list (a bare top-level text
  // node, or a stale index after an edit) and a composer with no located block:
  // render them at the end so nothing is silently dropped.
  for (const [bi, list] of cardsByBlock.value) {
    if (!placed.has(bi)) for (const t of list) out.push(renderCard(t));
  }
  if (pending.value && !placed.has(pendingBlock.value)) out.push(renderComposer());
  if (orphaned.value.length) out.push(renderOrphaned());
  return out;
};
</script>

<template>
  <div ref="containerEl" class="relative h-full w-full">
    <div ref="scrollerEl" class="h-full w-full overflow-auto bg-surface">
      <p
        v-if="error"
        class="m-4 rounded border border-block-line bg-block-soft p-3 text-sm text-block"
      >
        {{ error }}
      </p>
      <article
        ref="body"
        class="markdown-body mx-auto max-w-3xl px-6 py-5"
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
      class="btn-primary absolute z-20 gap-1 px-2 py-1 text-xs shadow-sm"
      data-testid="comment-select-button"
      :style="{ top: selectionButton.top + 'px', left: selectionButton.left + 'px' }"
      @mousedown.prevent
      @click="openComposer"
    >
      💬 Comment
    </button>
  </div>
</template>
