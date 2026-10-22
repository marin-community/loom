// Where sign-in returns a person. Most destinations are SPA routes, but the
// OAuth consent page that sends someone to sign in is served by the server,
// so it needs a full page load rather than a router navigation.

/** The one server-rendered page a person may be sent back to after signing in. */
const CONSENT_PATH = '/oauth/authorize';

/** A same-origin path, never `//host` or a backslash form a browser reads as another origin. */
export function isLocalPath(path: string): boolean {
  return path.startsWith('/') && !path.startsWith('//') && !path.includes('\\');
}

export type LoginDestination = { kind: 'server' | 'spa'; path: string };

/** Resolve the `redirect` query value to where sign-in should land. */
export function loginDestination(redirect: string | undefined): LoginDestination {
  if (!redirect || !isLocalPath(redirect)) return { kind: 'spa', path: '/' };
  const pathname = redirect.split(/[?#]/, 1)[0];
  if (pathname !== CONSENT_PATH) return { kind: 'spa', path: redirect };
  // Rebuilt on the constant path, so a full page load can reach only the
  // consent page; the rest is its query string.
  return { kind: 'server', path: CONSENT_PATH + redirect.slice(CONSENT_PATH.length) };
}

/** The GitHub sign-in link, carrying the destination through the round trip. */
export function githubLoginHref(base: string, redirect: string | undefined): string {
  const destination = loginDestination(redirect);
  return destination.path === '/' ? base : `${base}?next=${encodeURIComponent(destination.path)}`;
}
