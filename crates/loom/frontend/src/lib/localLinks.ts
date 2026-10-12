// Classification for the links an agent drops into prose (terminal output,
// conversation markdown, status messages) or a document carries: a href that
// points back at the loom server serving this page opens in the *existing*
// window — a known SPA route through the router, any other same-origin path
// as a plain same-window load — while every other origin keeps the new-tab
// treatment. One classifier, shared by the markdown renderer, the sanitizer
// hook, the document click router, the terminal, and the surfaced-links pane,
// so all those surfaces agree on what "local" means.

import type { Router } from 'vue-router';

/** The SPA-route form of `href` (path + query + hash) when it resolves against
 * `docUrl` back to that document's own origin, or null for in-page `#`
 * anchors, other origins, other schemes (mailto:, …), and unparseable hrefs.
 * Relative hrefs resolve the way the browser resolves them, against the
 * document URL — a `notes` link inside an artifact at `/s/x/artifacts/design`
 * is the artifact-deep-link `/s/x/artifacts/notes`. */
export function localRoute(href: string, docUrl: string): string | null {
  if (!href || href.startsWith('#')) return null;
  try {
    const here = new URL(docUrl);
    const url = new URL(href, here);
    if (url.origin !== here.origin) return null;
    return url.pathname + url.search + url.hash;
  } catch {
    return null;
  }
}

/** True when `href` points at the loom server this page was served from. */
export function isLocalHref(href: string): boolean {
  return localRoute(href, window.location.href) !== null;
}

/** Open `href` in the existing window when it points at this loom server: a
 * route the SPA knows goes through the router (no page reload), any other
 * same-origin path (e.g. an `/api/…` endpoint) as a full same-window load.
 * Returns true when the href was local and has been handled — the caller then
 * preventDefaults the click; false means it is external and belongs in a new
 * window. */
export function openLocalLink(router: Router, href: string): boolean {
  const route = localRoute(href, window.location.href);
  if (route == null) return false;
  if (router.resolve(route).matched.length > 0) router.push(route);
  else window.location.assign(href);
  return true;
}
