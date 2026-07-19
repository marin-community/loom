// markdown-it tokens → Vue vnodes.
//
// The old preview rendered markdown to an HTML *string* and dropped it into the
// article with `innerHTML` — an opaque blob Vue knew nothing about, which forced
// the comment layer to be hand-wired imperative DOM (listeners bolted on, cards
// teleported into spliced-in placeholders). This walks markdown-it's token
// stream into a real Vue tree instead, so the rendered document is Vue-owned:
// the comment layer interleaves thread cards as plain vnodes, and selection is a
// lifecycle-clean composable.
//
// The token stream already carries every projection the plugins apply (task
// lists, heading anchors, the smartdoc `#N`/`artifact:` chips — see
// `markdown.ts`), so this renderer stays generic: it maps each token to its HTML
// element honouring the token's own attributes, and the few things a plugin
// emits as literal HTML (`html_inline`/`html_block`) become small sanitised
// islands. Only fenced code is special-cased, routed to the CodeBlock /
// MermaidBlock leaf components (which lazy-load highlight.js / mermaid).

import { h, cloneVNode, isVNode, type VNode } from 'vue';
import type Token from 'markdown-it/lib/token.mjs';
import { resolveUrl, type RenderContext, type Sanitizer } from './markdown';
import CodeBlock from './components/CodeBlock.vue';
import MermaidBlock from './components/MermaidBlock.vue';

/** Everything the walker needs: the image/link resolution context plus the
 *  sanitiser for raw-HTML islands. */
export type RenderCtx = RenderContext & { sanitize: Sanitizer };

type Child = VNode | string;

/** markdown-it attrs (`[[name, value], …]`) → a Vue props object. */
function attrsToProps(attrs: [string, string][] | null): Record<string, unknown> {
  const props: Record<string, unknown> = {};
  if (attrs) for (const [k, v] of attrs) props[k] = v;
  return props;
}

/** Props for a generic open tag. Links get the new-tab treatment for off-page
 *  hrefs (matching the old DOMPurify hook): an in-page `#` anchor and the
 *  internal smartdoc SPA links (which arrive as `html_inline`, not `link_open`)
 *  are left same-tab. */
function elementProps(tok: Token): Record<string, unknown> {
  const props = attrsToProps(tok.attrs);
  if (tok.tag === 'a') {
    const href = typeof props.href === 'string' ? props.href : '';
    if (href && !href.startsWith('#')) {
      props.target = '_blank';
      props.rel = 'noopener noreferrer';
    }
  }
  return props;
}

interface Frame {
  /** The element tag, or null for a "transparent" frame (a hidden paragraph in a
   *  tight list) whose children splice straight into the parent. */
  tag: string | null;
  props: Record<string, unknown>;
  children: Child[];
}

/** Walk a (block or inline) token stream into vnodes. Recurses through `inline`
 *  tokens' children; a stack turns the flat open/close stream into a tree. */
function walk(tokens: Token[], ctx: RenderCtx): Child[] {
  const out: Child[] = [];
  const stack: Frame[] = [{ tag: '#root', props: {}, children: out }];
  const emit = (n: Child) => stack[stack.length - 1].children.push(n);

  for (const tok of tokens) {
    switch (tok.type) {
      case 'inline':
        for (const n of walk(tok.children ?? [], ctx)) emit(n);
        continue;
      case 'text':
        if (tok.content) emit(tok.content);
        continue;
      case 'softbreak':
        emit('\n');
        continue;
      case 'hardbreak':
        emit(h('br'));
        continue;
      case 'code_inline':
        emit(h('code', attrsToProps(tok.attrs), tok.content));
        continue;
      case 'fence':
      case 'code_block': {
        const lang = (tok.info || '').trim().split(/\s+/)[0] || '';
        emit(
          lang === 'mermaid'
            ? h(MermaidBlock, { code: tok.content })
            : h(CodeBlock, { code: tok.content, lang }),
        );
        continue;
      }
      case 'image': {
        const props = attrsToProps(tok.attrs);
        if (typeof props.src === 'string') props.src = resolveUrl(ctx, props.src);
        // The token's content is the flattened alt text.
        props.alt = tok.content;
        emit(h('img', props));
        continue;
      }
      case 'html_block':
        emit(h('div', { innerHTML: ctx.sanitize(tok.content) }));
        continue;
      case 'html_inline':
        emit(h('span', { innerHTML: ctx.sanitize(tok.content) }));
        continue;
      case 'hr':
        emit(h('hr', attrsToProps(tok.attrs)));
        continue;
    }

    // Generic open / close (paragraph, heading, list, blockquote, table cells,
    // strong, em, link, …).
    if (tok.nesting === 1) {
      const hidden = tok.hidden === true;
      stack.push({
        tag: hidden ? null : tok.tag,
        props: hidden ? {} : elementProps(tok),
        children: [],
      });
    } else if (tok.nesting === -1) {
      const frame = stack.pop();
      if (!frame) continue;
      if (frame.tag === null) {
        for (const c of frame.children) emit(c);
      } else {
        emit(h(frame.tag, frame.props, frame.children.length ? frame.children : undefined));
      }
    } else if (tok.content) {
      // An unhandled self-closing token — keep its text so nothing is lost.
      emit(tok.content);
    }
  }
  return out;
}

/** Render a block token stream into the top-level children of `.markdown-body`.
 *  Each top-level *element* block is stamped with `data-block="<index>"` so the
 *  comment layer can map a located anchor back to the block it sits under and
 *  interleave that block's thread cards right after it. */
export function renderTokens(tokens: Token[], ctx: RenderCtx): Child[] {
  return walk(tokens, ctx).map((n, i) =>
    isVNode(n) ? cloneVNode(n, { 'data-block': String(i) }) : n,
  );
}
