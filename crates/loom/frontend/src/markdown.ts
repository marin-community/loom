// Lazy-loaded Markdown rendering — a GitHub-flavoured viewer for the file
// browser's Preview mode.
//
// Everything heavy (markdown-it + plugins, highlight.js, DOMPurify, and
// especially mermaid) is pulled in via dynamic import the first time a markdown
// file is previewed, keeping it out of the main app chunk. This module is
// framework-agnostic; `MarkdownView.vue` drives it.

import type MarkdownIt from 'markdown-it';
// markdown-it v14's Token isn't re-exported from the package root; it's the
// default export of the `token` module (resolved via the types' `./*` exports).
import type Token from 'markdown-it/lib/token.mjs';
import type { IssueRefStatus } from './types';

/** Where to resolve a relative `![](…)` image to: the session's raw-bytes
 *  endpoint, with the path resolved against the markdown file's directory. */
export interface RenderContext {
  /** Session id, for building `/api/sessions/{id}/raw?path=…` URLs. */
  sessionId: string;
  /** Repo-relative path of the markdown file being rendered (its directory is
   *  the base for resolving relative image links). */
  filePath: string;
  /** Live status for `#N` issue references, keyed by id-as-string — the
   *  projection map (`ArtifactView.refs.issues`, or a client-built map for the
   *  goal). When present, smartdoc references become live chips/links; when
   *  absent, references render as plain text. An unkeyed `#N` (no entry) is left
   *  as plain text — only known issues get a chip. */
  refs?: Record<string, IssueRefStatus>;
}

// ---------------------------------------------------------------------------
// Path + URL helpers for relative image resolution
// ---------------------------------------------------------------------------

/** True for links we must leave untouched: absolute URLs, protocol-relative,
 *  root-absolute, fragments, and anything with a scheme (mailto:, data:, …). */
export function isExternal(url: string): boolean {
  return (
    /^[a-z][a-z0-9+.-]*:/i.test(url) || // scheme:  http:  data:  mailto:
    url.startsWith('//') ||
    url.startsWith('/') ||
    url.startsWith('#')
  );
}

/** Resolve `rel` against directory `dir` (both `/`-separated, repo-relative),
 *  collapsing `.`/`..` segments. Returns a clean repo-relative path. */
export function resolvePath(dir: string, rel: string): string {
  const stack = dir ? dir.split('/') : [];
  for (const part of rel.split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..') stack.pop();
    else stack.push(part);
  }
  return stack.join('/');
}

/** Map a markdown `src`/`href` to a viewable URL. Relative paths point at the
 *  raw-bytes endpoint so images in the worktree render inline; external links
 *  pass through unchanged. */
export function resolveUrl(ctx: RenderContext, url: string): string {
  if (!url || isExternal(url)) return url;
  const dir = ctx.filePath.includes('/')
    ? ctx.filePath.slice(0, ctx.filePath.lastIndexOf('/'))
    : '';
  const path = resolvePath(dir, url.replace(/^\.\//, ''));
  return `/api/sessions/${ctx.sessionId}/raw?path=${encodeURIComponent(path)}`;
}

// ---------------------------------------------------------------------------
// markdown-it instance (built once, lazily)
// ---------------------------------------------------------------------------

let mdPromise: Promise<MarkdownIt> | null = null;

async function getMarkdownIt(): Promise<MarkdownIt> {
  if (mdPromise) return mdPromise;
  mdPromise = (async () => {
    const [{ default: MarkdownItCtor }, hljsMod, taskLists, anchor] = await Promise.all([
      import('markdown-it'),
      import('highlight.js'),
      import('markdown-it-task-lists'),
      import('markdown-it-anchor'),
    ]);
    const hljs = hljsMod.default;

    const md = new MarkdownItCtor({
      html: true, // allow raw HTML (sanitised downstream by DOMPurify)
      linkify: true, // autolink bare URLs, GitHub-style
      typographer: true,
      breaks: false,
      highlight(code, lang): string {
        // Mermaid blocks are left as a placeholder `<pre class="mermaid">` for
        // the post-render pass to turn into an SVG diagram.
        if (lang === 'mermaid') {
          return `<pre class="mermaid">${md.utils.escapeHtml(code)}</pre>`;
        }
        if (lang && hljs.getLanguage(lang)) {
          try {
            const out = hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
            return `<pre class="hljs"><code class="language-${lang}">${out}</code></pre>`;
          } catch {
            /* fall through to the escaped default */
          }
        }
        return `<pre class="hljs"><code>${md.utils.escapeHtml(code)}</code></pre>`;
      },
    });

    // GitHub task lists (`- [ ]` / `- [x]`) render as disabled checkboxes.
    md.use(taskLists.default, { enabled: true, label: true });
    // smartdoc projection: `#N` issue refs → live status chips, `artifact:<name>`
    // → deep links. Reads the per-render refs map off `env.ctx`.
    installSmartdoc(md);
    // Slugged heading ids + a GitHub-style hover `#` permalink. In-page `#`
    // links are intercepted by MarkdownView so they scroll the preview rather
    // than hijacking the hash router.
    md.use(anchor.default, {
      slugify: (s: string) => slugify(s),
      permalink: anchor.default.permalink.linkInsideHeader({
        symbol: '#',
        placement: 'before',
        ariaHidden: true,
      }),
    });

    // Relative image sources point at the worktree's raw-bytes endpoint so
    // images committed alongside the doc render inline.
    const defaultImage =
      md.renderer.rules.image ??
      ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options));
    md.renderer.rules.image = (tokens, idx, options, env, self) => {
      const token = tokens[idx];
      const attr = token.attrIndex('src');
      const ctx = (env as { ctx?: RenderContext }).ctx;
      if (attr >= 0 && ctx && token.attrs) {
        token.attrs[attr][1] = resolveUrl(ctx, token.attrs[attr][1]);
      }
      return defaultImage(tokens, idx, options, env, self);
    };

    return md;
  })();
  return mdPromise;
}

// ---------------------------------------------------------------------------
// smartdoc projection — `#N` issue refs become live status chips, `artifact:N`
// refs become deep links. Status comes from the refs map (the live ledger),
// never from the text. Implemented as a markdown-it *core* rule that rewrites
// only `text` inline tokens, so a `#123` inside a code span (`code_inline`
// token) or a fenced/indented code block (block-level tokens with no inline
// children) is left untouched — matching the backend's smartdoc parser.
// ---------------------------------------------------------------------------

/** A reference smartdoc recognises inside running prose. */
type SmartRef =
  { kind: 'issue'; raw: string; id: number } | { kind: 'artifact'; raw: string; name: string };

// `#123` (issue) or `artifact:<name>`. The issue form requires a non-word char
// (or string start) before the `#` so `foo#3` / `abc#1` (anchors, fragments)
// don't match; the artifact name runs to the first whitespace/closing punct.
const SMARTDOC_RE = /(^|[^\w/&])#(\d+)\b|\bartifact:([A-Za-z0-9._-]+)/g;

/** Issue chip → token color mapping, consistent with how issues are badged
 *  elsewhere (open = neutral, claimed = amber/attention, closed = accent). */
function issueChipClass(s: IssueRefStatus): string {
  if (s.status === 'closed') return 'smartdoc-chip smartdoc-chip--closed';
  if (s.claimed_branch) return 'smartdoc-chip smartdoc-chip--claimed';
  return 'smartdoc-chip smartdoc-chip--open';
}

function issueChipLabel(s: IssueRefStatus): string {
  if (s.status === 'closed') return 'closed';
  if (s.claimed_branch) return `claimed · ${s.claimed_branch}`;
  return 'open';
}

/** Build the HTML for one recognised reference, given the live refs map and the
 *  rendering context (for the artifact deep-link base). Returns null when the
 *  reference can't be projected (unknown issue, no session) — the caller then
 *  leaves the raw text in place. `esc` is markdown-it's HTML escaper. */
function refHtml(ref: SmartRef, ctx: RenderContext, esc: (s: string) => string): string | null {
  if (ref.kind === 'issue') {
    const status = ctx.refs?.[String(ref.id)];
    if (!status) return null;
    const cls = issueChipClass(status);
    const title = `#${status.id} · ${issueChipLabel(status)} — ${status.title}`;
    // Issue chips link out to the cross-repo Issues pane, scrolled to the id.
    return (
      `<a class="${cls}" href="/issues#issue-${status.id}" ` +
      `data-issue="${status.id}" data-status="${esc(status.status)}" ` +
      `title="${esc(title)}">#${status.id}</a>`
    );
  }
  // artifact:<name> → deep link to that artifact's surface.
  const href = `/s/${encodeURIComponent(ctx.sessionId)}/artifacts/${encodeURIComponent(ref.name)}`;
  return `<a class="smartdoc-artifact" href="${href}" data-artifact="${esc(ref.name)}">${esc(ref.raw)}</a>`;
}

/** Register the smartdoc core rule on a markdown-it instance. */
function installSmartdoc(md: MarkdownIt): void {
  md.core.ruler.push('smartdoc_refs', (state) => {
    const ctx = (state.env as { ctx?: RenderContext }).ctx;
    // No projection map → nothing to chip; leave the doc as plain markdown.
    if (!ctx) return;
    for (const block of state.tokens) {
      if (block.type !== 'inline' || !block.children) continue;
      const next: typeof block.children = [];
      for (const tok of block.children) {
        // Only plain text is eligible — never code spans, link text we already
        // built, or other inline structure.
        if (tok.type !== 'text') {
          next.push(tok);
          continue;
        }
        const out = splitTextToken(md, tok, ctx);
        for (const t of out) next.push(t);
      }
      block.children = next;
    }
  });
}

/** Split one `text` token into a run of `text`/`html_inline` tokens, replacing
 *  recognised references with chip/link HTML. Tokens that aren't projectable are
 *  left as text so nothing is lost. */
function splitTextToken(md: MarkdownIt, tok: Token, ctx: RenderContext): Token[] {
  const text = tok.content;
  const esc = md.utils.escapeHtml;
  // markdown-it doesn't export Token as a value here; reuse the token ctor via
  // the live token's prototype so we don't need the class import.
  const TokenCtor = tok.constructor as new (type: string, tag: string, nesting: number) => Token;
  SMARTDOC_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  let last = 0;
  const out: Token[] = [];
  const pushText = (s: string) => {
    if (!s) return;
    const t = new TokenCtor('text', '', 0);
    t.content = s;
    out.push(t);
  };

  while ((m = SMARTDOC_RE.exec(text)) !== null) {
    const [whole, lead, issueId, artifactName] = m;
    // The leading char (issue form) isn't part of the ref — keep it as text.
    const refStart = m.index + (lead ? lead.length : 0);
    let ref: SmartRef;
    if (issueId != null) {
      ref = { kind: 'issue', raw: `#${issueId}`, id: Number(issueId) };
    } else {
      ref = { kind: 'artifact', raw: `artifact:${artifactName}`, name: artifactName };
    }
    const html = refHtml(ref, ctx, esc);
    if (html == null) {
      // Not projectable — advance past this match without splitting.
      continue;
    }
    pushText(text.slice(last, refStart));
    const h = new TokenCtor('html_inline', '', 0);
    h.content = html;
    out.push(h);
    last = m.index + whole.length;
  }
  if (out.length === 0) return [tok]; // nothing matched — keep the original
  pushText(text.slice(last));
  return out;
}

/** GitHub-ish heading slug: lowercase, spaces to hyphens, drop punctuation. */
function slugify(s: string): string {
  return s
    .trim()
    .toLowerCase()
    .replace(/[^\w\- ]+/g, '')
    .replace(/\s+/g, '-');
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/** Parse markdown source into markdown-it's token stream, with every projection
 *  plugin applied (task lists, heading anchors, the smartdoc `#N`/`artifact:`
 *  refs). The vnode renderer (`markdown-render.ts`) walks these tokens into a
 *  real Vue tree — no intermediate HTML string, so the rendered document is
 *  Vue-owned (the comment layer interleaves as real vnodes, not teleported into
 *  an innerHTML blob). `ctx` rides on `env.ctx`, where the smartdoc core rule
 *  reads the refs map. */
export async function parseMarkdown(src: string, ctx: RenderContext): Promise<Token[]> {
  const md = await getMarkdownIt();
  return md.parse(src, { ctx });
}

/** Sanitises the raw-HTML islands the vnode renderer emits: markdown that embeds
 *  literal HTML (`html: true`), plus the chips/checkboxes the smartdoc, anchor,
 *  and task-list plugins produce as `html_inline`. Same policy as the old
 *  whole-document sanitise — external links open in a new tab with the opener
 *  neutralised, and the smartdoc `data-*` / `target` attributes survive. The link
 *  hook is added and removed around each call so it never bleeds into another
 *  DOMPurify user (e.g. MermaidBlock's SVG sanitise). */
export type Sanitizer = (html: string) => string;

let sanitizerPromise: Promise<Sanitizer> | null = null;
export function loadSanitizer(): Promise<Sanitizer> {
  if (sanitizerPromise) return sanitizerPromise;
  sanitizerPromise = (async () => {
    const DOMPurify = (await import('dompurify')).default;
    const linkHook = (node: Element) => {
      if (node.tagName !== 'A') return;
      const href = node.getAttribute('href');
      const internal = node.hasAttribute('data-issue') || node.hasAttribute('data-artifact');
      if (href && !href.startsWith('#') && !internal) {
        node.setAttribute('target', '_blank');
        node.setAttribute('rel', 'noopener noreferrer');
      }
    };
    return (html: string) => {
      DOMPurify.addHook('afterSanitizeAttributes', linkHook);
      try {
        // Defaults already allow the tags/attrs the plugins emit (`a`, `span`,
        // `input[type=checkbox]`, `label`, `class`, `id`); `target` plus the
        // smartdoc `data-*` hooks keep the new-tab and SPA-link interception.
        return DOMPurify.sanitize(html, {
          ADD_ATTR: ['target', 'data-issue', 'data-status', 'data-artifact'],
        });
      } finally {
        DOMPurify.removeHook('afterSanitizeAttributes');
      }
    };
  })();
  return sanitizerPromise;
}
