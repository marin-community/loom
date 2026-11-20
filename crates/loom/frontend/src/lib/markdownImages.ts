// Classification of a markdown image source for the URL resolver in
// `markdown.ts`: where a source points once the document-rendering context
// (its directory) is known. Pure string work with no app imports, so the unit
// tests run under plain node.

/** Where an image source points once classified against a document. */
export type ImageSource =
  | { kind: 'artifact'; name: string }
  /** An absolute path spelling inside the session's worktree (`/…`, `file:///…`);
   *  served by `sessions.image`, which confines reads to the worktree. */
  | { kind: 'host'; path: string }
  /** Repo-relative, resolved against the document's directory. */
  | { kind: 'worktree'; path: string }
  /** Left untouched: absolute URLs, `data:` URIs, loom's own `/api/…` paths. */
  | { kind: 'external'; url: string };

/** True for artifact names loom's store accepts as image sources. */
export function isArtifactName(name: string): boolean {
  return name !== '.' && name !== '..' && /^[A-Za-z0-9._-]+$/.test(name);
}

/** True for link forms we must leave untouched: absolute URLs, protocol-
 *  relative, fragments, and anything with a scheme (mailto:, data:, …).
 *  Root-absolute `/…` paths never reach this check from the classifier — they
 *  are host paths there — but they remain "external" to any other caller. */
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

/** Strip the query and fragment from a markdown URL and percent-decode its
 *  path part. Returns null when the encoding is malformed. */
function decodePath(url: string): string | null {
  try {
    return decodeURIComponent(url.split(/[?#]/, 1)[0]);
  } catch {
    return null;
  }
}

/** The host path a `file://` URL names, or null for a non-local form
 *  (`file://server/share` points elsewhere). */
function filePath(url: string): string | null {
  const rest = url.slice('file://'.length);
  const [authority, ...segments] = rest.split('/');
  if (authority && authority !== 'localhost') return null;
  return decodePath(`/${segments.join('/')}`);
}

/** Classify a markdown image source for a document whose repo-relative path
 *  anchors relative references (its directory). Allow-list shaped: only the
 *  explicit forms — `artifact:<name>`, `file:///…`, root-absolute `/…`, and
 *  plain relative paths — are rewritten to a loom-served URL; everything else
 *  (schemes, protocol-relative URLs, fragments, malformed encodings) passes
 *  through as `external`, untouched. Rewrites can only point at
 *  `sessions.image`, which re-checks path confinement and content sniffing
 *  itself, so nothing here is trusted for security. */
export function classifyImageSource(dir: string, url: string): ImageSource {
  if (url.startsWith('artifact:')) {
    const name = url.slice('artifact:'.length);
    if (isArtifactName(name)) return { kind: 'artifact', name };
  }
  if (url.startsWith('file://')) {
    const path = filePath(url);
    if (path) return { kind: 'host', path };
  } else if (url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/api/')) {
    // A root-absolute path from agent-authored prose is a worktree path, not a
    // web path — but protocol-relative URLs and URLs that already target this
    // loom server's own API are left to request directly. Genuinely-served
    // same-origin assets (the SPA's static files) are rare in prose; when they
    // do appear, the fallback card names them.
    const path = decodePath(url);
    if (path) return { kind: 'host', path };
  }
  if (!url || isExternal(url)) return { kind: 'external', url };
  const rel = decodePath(url);
  if (rel == null) return { kind: 'external', url };
  return { kind: 'worktree', path: resolvePath(dir, rel) };
}
