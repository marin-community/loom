// Where sign-in returns a person. Most destinations are SPA routes, but the
// OAuth consent page that sends someone to sign in is served by the server,
// so it needs a full page load rather than a router navigation.

/** Server-rendered paths a person may be sent back to after signing in. */
const SERVER_PATHS = ['/oauth/authorize'];

/** A same-origin path, never `//host` or a backslash form a browser reads as another origin. */
export function isLocalPath(path: string): boolean {
  return path.startsWith('/') && !path.startsWith('//') && !path.includes('\\');
}

export type LoginDestination = { kind: 'server' | 'spa'; path: string };

/** Resolve the `redirect` query value to where sign-in should land. */
export function loginDestination(redirect: string | undefined): LoginDestination {
  if (!redirect || !isLocalPath(redirect)) return { kind: 'spa', path: '/' };
  const pathname = redirect.split(/[?#]/, 1)[0];
  return SERVER_PATHS.includes(pathname)
    ? { kind: 'server', path: redirect }
    : { kind: 'spa', path: redirect };
}

/** The GitHub sign-in link, carrying the destination through the round trip. */
export function githubLoginHref(base: string, redirect: string | undefined): string {
  const destination = loginDestination(redirect);
  return destination.path === '/' ? base : `${base}?next=${encodeURIComponent(destination.path)}`;
}
