// Lazy-loaded Markdown rendering — a GitHub-flavoured viewer for the file
// browser's Preview mode.
//
// Everything heavy (markdown-it + plugins, highlight.js, DOMPurify, and
// especially mermaid) is pulled in via dynamic import the first time a markdown
// file is previewed, keeping it out of the main app chunk exactly as `monaco.ts`
// does for the editor. This module is framework-agnostic; `MarkdownView.vue`
// drives it.

import type MarkdownIt from 'markdown-it';

/** Where to resolve a relative `![](…)` image to: the session's raw-bytes
 *  endpoint, with the path resolved against the markdown file's directory. */
export interface RenderContext {
  /** Session id, for building `/api/sessions/{id}/raw?path=…` URLs. */
  sessionId: string;
  /** Repo-relative path of the markdown file being rendered (its directory is
   *  the base for resolving relative image links). */
  filePath: string;
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
  // (a real navigation would clobber the hash router).
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    const href = node.tagName === 'A' ? node.getAttribute('href') : null;
    if (href && !href.startsWith('#')) {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
  try {
    const html = md.render(src, { ctx });
    // Defaults already allow the tags/attrs we emit (`pre`, `code`, `span`,
    // `input[type=checkbox]`, `class`, `id`); `target` is added so the
    // new-tab hook above sticks.
    return DOMPurify.sanitize(html, { ADD_ATTR: ['target'] });
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
      // for defence-in-depth (and to keep the flow provably XSS-safe). The svg +
      // html profiles keep mermaid's structure, incl. `<foreignObject>` labels.
      wrap.innerHTML = DOMPurify.sanitize(svg, {
        USE_PROFILES: { svg: true, svgFilters: true, html: true },
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
