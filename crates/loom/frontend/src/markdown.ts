// Lazy-loaded Markdown rendering — a GitHub-flavoured viewer for the file
// browser's Preview mode.
//
// Everything heavy (markdown-it + plugins, highlight.js, DOMPurify, and
// especially mermaid) is pulled in via dynamic import the first time a markdown
// file is previewed, keeping it out of the main app chunk exactly as `monaco.ts`
// does for the editor. This module is framework-agnostic; `MarkdownView.vue`
// drives it.

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
function isExternal(url: string): boolean {
  return (
    /^[a-z][a-z0-9+.-]*:/i.test(url) || // scheme:  http:  data:  mailto:
    url.startsWith('//') ||
    url.startsWith('/') ||
    url.startsWith('#')
  );
}

/** Resolve `rel` against directory `dir` (both `/`-separated, repo-relative),
 *  collapsing `.`/`..` segments. Returns a clean repo-relative path. */
function resolvePath(dir: string, rel: string): string {
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
function resolveUrl(ctx: RenderContext, url: string): string {
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
  | { kind: 'issue'; raw: string; id: number }
  | { kind: 'artifact'; raw: string; name: string };

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
function refHtml(
  ref: SmartRef,
  ctx: RenderContext,
  esc: (s: string) => string,
): string | null {
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
function splitTextToken(
  md: MarkdownIt,
  tok: Token,
  ctx: RenderContext,
): Token[] {
  const text = tok.content;
  const esc = md.utils.escapeHtml;
  // markdown-it doesn't export Token as a value here; reuse the token ctor via
  // the live token's prototype so we don't need the class import.
  const TokenCtor = tok.constructor as new (
    type: string,
    tag: string,
    nesting: number,
  ) => Token;
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

/** Render markdown source to sanitised HTML, resolving relative images against
 *  the file's location. Mermaid code blocks come back as `<pre class="mermaid">`
 *  placeholders — call {@link renderMermaid} on the mounted element to draw them. */
export async function renderMarkdown(src: string, ctx: RenderContext): Promise<string> {
  const [md, purifyMod] = await Promise.all([getMarkdownIt(), import('dompurify')]);
  const DOMPurify = purifyMod.default;

  // Open off-page links in a new tab and neutralise the opener. In-page `#`
  // anchors are left alone — MarkdownView intercepts them to scroll the preview
  // (a real navigation would clobber the hash router). smartdoc chips/links
  // (carrying `data-issue` / `data-artifact`) are internal SPA routes —
  // MarkdownView intercepts them too, so they stay same-tab.
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName !== 'A') return;
    const href = node.getAttribute('href');
    const internal = node.hasAttribute('data-issue') || node.hasAttribute('data-artifact');
    if (href && !href.startsWith('#') && !internal) {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
  try {
    const html = md.render(src, { ctx });
    // Defaults already allow the tags/attrs we emit (`pre`, `code`, `span`,
    // `input[type=checkbox]`, `class`, `id`); `target` plus the smartdoc
    // `data-*` hooks are added so the new-tab hook and the SPA-link interception
    // stick.
    return DOMPurify.sanitize(html, { ADD_ATTR: ['target', 'data-issue', 'data-status', 'data-artifact'] });
  } finally {
    DOMPurify.removeHook('afterSanitizeAttributes');
  }
}

let mermaidSeq = 0;

// Mermaid's stock `default`/`dark` themes paint nodes lavender-purple, which
// clashes with loom's neutral-slate + single-blue palette. Drive mermaid's
// `base` theme with loom's own tokens instead so diagrams read as part of the
// same UI in both palettes. Hexes mirror the slate/blue values in styles.css.
function mermaidThemeVariables(dark: boolean): Record<string, string> {
  return dark
    ? {
        background: '#1e293b', // surface (slate-800)
        primaryColor: '#0f172a', // canvas (slate-900) — node fill
        primaryBorderColor: '#475569', // slate-600
        primaryTextColor: '#f1f5f9', // slate-100
        lineColor: '#64748b', // slate-500 — edges
        secondaryColor: '#334155', // slate-700
        tertiaryColor: '#0f172a',
      }
    : {
        background: '#ffffff',
        primaryColor: '#f1f5f9', // slate-100 — node fill
        primaryBorderColor: '#cbd5e1', // slate-300
        primaryTextColor: '#0f172a', // slate-900
        lineColor: '#94a3b8', // slate-400 — edges
        secondaryColor: '#e2e8f0', // slate-200
        tertiaryColor: '#f8fafc',
      };
}

/** Turn every `<pre class="mermaid">` placeholder under `root` into a rendered
 *  SVG diagram. Mermaid runs with `securityLevel: 'strict'` (it sanitises its
 *  own SVG); a diagram that fails to parse is left as its source text with an
 *  error note rather than blowing up the whole preview. */
export async function renderMermaid(root: HTMLElement, dark: boolean): Promise<void> {
  const blocks = Array.from(root.querySelectorAll<HTMLElement>('pre.mermaid'));
  if (blocks.length === 0) return;
  const [mermaid, purifyMod] = await Promise.all([
    import('mermaid').then((m) => m.default),
    import('dompurify'),
  ]);
  const DOMPurify = purifyMod.default;
  // initialize is idempotent-ish but theme can change with the app theme, so
  // re-apply it each pass.
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: 'base',
    themeVariables: mermaidThemeVariables(dark),
    fontFamily: 'inherit',
    // Throw on a bad diagram instead of drawing mermaid's "bomb" error graphic.
    // The default renders that graphic into a temp node mermaid appends to
    // `document.body` and only removes on success — so a failed parse leaks the
    // bomb into the page body, where it survives route changes and shows up at
    // the bottom of every view. Suppressing it makes `render` reject cleanly
    // (mermaid removes its temp node), and the catch below shows our own inline
    // error note instead.
    suppressErrorRendering: true,
  });

  for (const block of blocks) {
    const code = block.textContent ?? '';
    const id = `weaver-mermaid-${mermaidSeq++}`;
    try {
      const { svg } = await mermaid.render(id, code);
      const wrap = document.createElement('div');
      wrap.className = 'mermaid-diagram';
      // Mermaid already sanitises under `securityLevel: 'strict'`, but the SVG
      // is derived from DOM text, so pass it through DOMPurify before innerHTML
      // for defence-in-depth (and to keep the flow provably XSS-safe).
      //
      // Mermaid draws node/cluster labels as HTML inside `<foreignObject>`. That
      // only survives sanitising if DOMPurify is told `foreignobject` is an HTML
      // integration point (otherwise its XHTML children are dropped as stray SVG
      // and every label renders blank). These options mirror mermaid's own
      // internal sanitise call, so we keep its structure verbatim while still
      // stripping scripts/handlers.
      wrap.innerHTML = DOMPurify.sanitize(svg, {
        ADD_TAGS: ['foreignobject'],
        ADD_ATTR: ['dominant-baseline'],
        HTML_INTEGRATION_POINTS: { foreignobject: true },
      });
      block.replaceWith(wrap);
    } catch (e) {
      const err = document.createElement('div');
      err.className = 'mermaid-error';
      err.textContent = `Could not render mermaid diagram: ${(e as Error).message}`;
      block.before(err);
      // Leave the source block in place so the content isn't lost.
    }
  }
}
