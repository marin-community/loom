// Shared plumbing for the two markdown document surfaces — MarkdownView (the
// read-only preview used on Overview / Conversation) and ArtifactDocument (the
// commentable artifact surface). Both parse the same source into the same token
// stream and route in-document link clicks the same way; only what they do
// *after* a build (emit vs. locate comment threads) and *besides* a link click
// (nothing vs. a caret hit-test to focus a thread) differs. This keeps that one
// pipeline in one place.

import { ref, shallowRef, watch, onMounted, nextTick } from 'vue';
import type { Router } from 'vue-router';
import { parseMarkdown, loadSanitizer, type RenderContext } from '../markdown';
import type { RenderCtx } from '../markdown-render';
import type Token from 'markdown-it/lib/token.mjs';
import type { IssueRefStatus } from '../types';

/** The props both surfaces share (each may carry more). */
export interface MarkdownDocProps {
  /** Session id — resolves relative images to the raw-bytes endpoint and builds
   *  `artifact:` deep links. */
  id: string;
  /** Repo-relative path of the markdown (its directory anchors images). */
  path: string;
  /** Raw markdown source. */
  source: string;
  /** Live `#N` issue status map for the smartdoc projection. */
  refs?: Record<string, IssueRefStatus>;
}

/**
 * The markdown build pipeline: parse `source` → tokens, load the sanitiser, and
 * expose reactive `tokens`/`ctx` that drive the vnode renderer. A monotonic
 * `runId` guards against an out-of-order async finish (a newer source landing
 * while an older parse is mid-flight). Registers its own source watch + initial
 * build; `onBuilt` fires once the fresh tree has painted (`nextTick`), receiving
 * the rendered `<article>` element.
 */
export function useMarkdownDoc(
  props: MarkdownDocProps,
  onBuilt?: (body: HTMLElement | null) => void,
) {
  const body = ref<HTMLElement | null>(null);
  const error = ref('');
  // shallowRef: replaced wholesale each build, never mutated in place.
  const tokens = shallowRef<Token[]>([]);
  const ctx = shallowRef<RenderCtx | null>(null);

  const renderContext = (): RenderContext => ({
    sessionId: props.id,
    filePath: props.path,
    refs: props.refs,
  });

  let runId = 0;
  async function build() {
    const mine = ++runId;
    try {
      const [toks, sanitize] = await Promise.all([
        parseMarkdown(props.source, renderContext()),
        loadSanitizer(),
      ]);
      if (mine !== runId) return;
      ctx.value = { ...renderContext(), sanitize };
      tokens.value = toks;
      error.value = '';
      await nextTick();
      if (mine !== runId) return;
      onBuilt?.(body.value);
    } catch (e) {
      if (mine === runId) error.value = (e as Error).message;
    }
  }

  watch(() => [props.source, props.path, props.refs], build, { deep: true });
  onMounted(build);

  return { body, error, tokens, ctx, build, renderContext };
}

/**
 * Route an in-document link click. Heading permalinks / in-doc `#` anchors scroll
 * the target into view (never touching the SPA's hash router); smartdoc chips
 * (`data-issue` / `data-artifact`) push their internal route. Returns true when
 * the click was a document link we handled — so a caller can then fall through to
 * its own behaviour (e.g. a comment hit-test) only on a plain, non-link click.
 */
export function routeDocLink(e: MouseEvent, router: Router, body: HTMLElement | null): boolean {
  const anchor = (e.target as HTMLElement).closest('a');
  if (!anchor) return false;
  const href = anchor.getAttribute('href');
  if (!href) return true; // an <a> with no href: still a link target, swallow it
  if (anchor.hasAttribute('data-issue') || anchor.hasAttribute('data-artifact')) {
    e.preventDefault();
    router.push(href);
    return true;
  }
  if (!href.startsWith('#')) return true; // external/other: let the <a> do its thing
  e.preventDefault();
  const id = decodeURIComponent(href.slice(1));
  body?.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({
    behavior: 'smooth',
    block: 'start',
  });
  return true;
}
