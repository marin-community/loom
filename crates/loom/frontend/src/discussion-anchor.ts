// Stand-off text anchoring + CSS Custom Highlight painting for artifact comment
// threads.
//
// A thread lives in the database anchored by a *quoted span* (a W3C-style
// text-quote selector: the quote plus a little surrounding context), not by a
// character offset into the markdown source. We locate that span in the
// *rendered* markdown DOM at view time and paint it with the CSS Custom
// Highlight API — no wrapper elements are inserted, so re-rendering the markdown
// (mermaid, theme flips, an edit elsewhere) never fights the highlights, and an
// anchor survives edits made outside its span. When the quote can no longer be
// found the thread is *orphaned* (still readable in the rail, just not painted).
//
// Everything here works in a flattened text space: the rendered subtree's text
// nodes concatenated into one string, with a map back to (node, localOffset).
// Capturing and locating both operate on that identical string, so a fresh
// capture round-trips to an exact substring match.

/** A thread's anchor as the API models it (`AnchorDto`). */
export interface TextAnchor {
  quote: string;
  prefix: string;
  suffix: string;
}

/** Chars of surrounding context captured on each side for disambiguation. */
const CONTEXT = 32;

/** Marks an element as comment-layer chrome inserted into the rendered body, not
 *  document content: anchoring skips the text inside a marked element (so a
 *  card's own echo of a quote is never matched), and the layer stamps and finds
 *  its placeholders by this attribute. One shared constant so the write and the
 *  read can't silently desync on a typo. */
export const COMMENT_UI_ATTR = 'data-weaver-comment-ui';

// `Highlight` / `HighlightRegistry` / `CSS.highlights` are declared by the DOM
// lib (lib.dom.d.ts) — we use them directly; `supportsHighlights` guards the
// runtime where the API is absent.

const HL_NAME = 'weaver-comment';
const HL_ACTIVE = 'weaver-comment-active';

// ---------------------------------------------------------------------------
// Flatten the rendered subtree to a string + an offset→text-node map.
// ---------------------------------------------------------------------------

interface NodeSpan {
  node: Text;
  start: number; // inclusive flattened offset of this node's first char
  end: number; // exclusive flattened offset (start + node length)
}

interface TextMap {
  text: string;
  spans: NodeSpan[];
}

function buildTextMap(root: HTMLElement): TextMap {
  // Skip text living inside the inline comment UI we teleport into the rendered
  // body (thread cards, composers, the unanchored footer). It isn't document
  // content, and letting it into the flattened text would let a quote match
  // against a card's own echo of that quote on the next locate pass.
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(n) {
      return n.parentElement?.closest(`[${COMMENT_UI_ATTR}]`)
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT;
    },
  });
  const spans: NodeSpan[] = [];
  let text = '';
  let node = walker.nextNode() as Text | null;
  while (node) {
    const start = text.length;
    text += node.data;
    spans.push({ node, start, end: text.length });
    node = walker.nextNode() as Text | null;
  }
  return { text, spans };
}

/** The flattened offset of a DOM boundary `(container, offset)`. Handles both a
 *  Text container (the common case — a selection inside prose) and an Element
 *  container (offset is a child index, e.g. a paragraph-level triple-click),
 *  descending to the nearest text node. */
function boundaryOffset(map: TextMap, container: Node, offset: number): number {
  if (container.nodeType === Node.TEXT_NODE) {
    const span = map.spans.find((s) => s.node === container);
    return span ? span.start + offset : 0;
  }
  // Element boundary: `offset` indexes childNodes. Resolve to the flattened
  // position at the first text node at/after that child (or end-of-subtree).
  const el = container as Element;
  const child = el.childNodes[offset];
  if (child) {
    // Start of the first text node within `child` (inclusive of `child` itself).
    for (const s of map.spans) {
      if (child === s.node || (child.contains && child.contains(s.node))) {
        return s.start;
      }
    }
  }
  // No text node at/after the boundary → the end of the subtree preceding it.
  const prev = el.childNodes[offset - 1];
  if (prev) {
    for (let i = map.spans.length - 1; i >= 0; i--) {
      const s = map.spans[i];
      if (prev === s.node || (prev.contains && prev.contains(s.node))) {
        return s.end;
      }
    }
  }
  return map.text.length;
}

/** Build a DOM Range spanning the flattened offset window `[from, to)`. */
function rangeForWindow(map: TextMap, from: number, to: number): Range | null {
  if (!map.spans.length) return null;
  const locus = (off: number, preferStart: boolean) => {
    for (const s of map.spans) {
      // For a start boundary at a node seam prefer the *next* node's start; for
      // an end boundary prefer the current node's end (off <= s.end).
      if (preferStart ? off >= s.start && off < s.end : off > s.start && off <= s.end) {
        return { node: s.node, local: off - s.start };
      }
    }
    // Exact-seam / clamp fallbacks.
    if (preferStart) {
      const s = map.spans.find((s) => off <= s.start) ?? map.spans[map.spans.length - 1];
      return { node: s.node, local: Math.max(0, off - s.start) };
    }
    const s = [...map.spans].reverse().find((s) => off >= s.end) ?? map.spans[0];
    return { node: s.node, local: Math.min(s.node.data.length, off - s.start) };
  };
  const s = locus(from, true);
  const e = locus(to, false);
  try {
    const range = document.createRange();
    range.setStart(s.node, Math.min(s.local, s.node.data.length));
    range.setEnd(e.node, Math.min(e.local, e.node.data.length));
    return range;
  } catch {
    return null;
  }
}

// --- context scoring for disambiguation ------------------------------------

function commonSuffix(a: string, b: string): number {
  let n = 0;
  while (n < a.length && n < b.length && a[a.length - 1 - n] === b[b.length - 1 - n]) n++;
  return n;
}

function commonPrefix(a: string, b: string): number {
  let n = 0;
  while (n < a.length && n < b.length && a[n] === b[n]) n++;
  return n;
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/** Capture an anchor from a live selection Range inside `root`. Returns null for
 *  an empty/collapsed selection or one outside the rendered body. The quote and
 *  context are read from the same flattened text space `locate` searches, so the
 *  anchor round-trips to an exact match on the revision it was taken from. */
export function captureAnchor(root: HTMLElement, range: Range): TextAnchor | null {
  if (range.collapsed) return null;
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;
  const map = buildTextMap(root);
  const from = boundaryOffset(map, range.startContainer, range.startOffset);
  const to = boundaryOffset(map, range.endContainer, range.endOffset);
  const lo = Math.min(from, to);
  const hi = Math.max(from, to);
  const quote = map.text.slice(lo, hi).trim();
  if (!quote) return null;
  // Recompute the trimmed window so prefix/suffix abut the trimmed quote.
  const start = map.text.indexOf(quote, lo);
  const s = start === -1 ? lo : start;
  const end = s + quote.length;
  return {
    quote,
    prefix: map.text.slice(Math.max(0, s - CONTEXT), s),
    suffix: map.text.slice(end, end + CONTEXT),
  };
}

/** Locate an anchor's span in the current rendered body, or null if the quote is
 *  gone (the thread is orphaned). Every occurrence of the quote is scored by how
 *  much of the recorded prefix/suffix still surrounds it, so the right instance
 *  is chosen when the quote repeats. */
export function locate(root: HTMLElement, anchor: TextAnchor): Range | null {
  const quote = anchor.quote?.trim();
  if (!quote) return null;
  const map = buildTextMap(root);
  let best = -1;
  let bestScore = -1;
  for (let idx = map.text.indexOf(quote); idx !== -1; idx = map.text.indexOf(quote, idx + 1)) {
    const before = map.text.slice(Math.max(0, idx - anchor.prefix.length), idx);
    const after = map.text.slice(idx + quote.length, idx + quote.length + anchor.suffix.length);
    const score = commonSuffix(before, anchor.prefix) + commonPrefix(after, anchor.suffix);
    if (score > bestScore) {
      bestScore = score;
      best = idx;
    }
  }
  if (best === -1) return null;
  return rangeForWindow(map, best, best + quote.length);
}

/** The top-level block element within `root` (a direct child — paragraph,
 *  heading, list, code fence, …) that contains `node`, or null if `node` is a
 *  direct text child of `root` or lies outside it. The inline comment layer
 *  inserts each thread's card as a sibling right after this block, so the
 *  discussion reads directly under the prose it annotates. */
export function blockContaining(root: HTMLElement, node: Node): HTMLElement | null {
  if (!root.contains(node)) return null;
  let el: HTMLElement | null =
    node.nodeType === Node.TEXT_NODE ? node.parentElement : (node as HTMLElement);
  if (!el || el === root) return null;
  while (el.parentElement && el.parentElement !== root) el = el.parentElement;
  return el.parentElement === root ? el : null;
}

/** Whether the browser supports the CSS Custom Highlight API. When false, the
 *  inline comment cards still work; spans just aren't tinted. */
export function supportsHighlights(): boolean {
  return typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';
}

/** Paint every located span, plus (optionally) the focused thread's span in the
 *  active tint. Replaces any previous paint. No-op when unsupported. */
export function paintHighlights(all: Range[], active: Range | null): void {
  if (!supportsHighlights()) return;
  const { highlights } = CSS;
  if (all.length) highlights.set(HL_NAME, new Highlight(...all));
  else highlights.delete(HL_NAME);
  if (active) highlights.set(HL_ACTIVE, new Highlight(active));
  else highlights.delete(HL_ACTIVE);
}

/** Drop all comment highlights (e.g. when leaving preview, or clearing state). */
export function clearHighlights(): void {
  if (!supportsHighlights()) return;
  CSS.highlights.delete(HL_NAME);
  CSS.highlights.delete(HL_ACTIVE);
}
