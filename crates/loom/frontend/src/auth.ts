// The client-side identity store: one reactive `me` mirroring `GET /api/auth/me`.
// Views read `me.authenticated` to decide chrome (App.vue) and the router guard
// (main.ts) reads it to gate navigation. A loopback-trusted user comes back
// authenticated with no login step; a remote user starts unauthenticated until
// they sign in.
import { reactive } from 'vue';
import * as api from './api';
import type { Me } from './types';

const EMPTY: Me = {
  authenticated: false,
  username: null,
  github_login: null,
  via: null,
  role: null,
  authorization_kind: null,
  methods: { password: true, github: false },
};

export const me = reactive<Me>({ ...EMPTY });

function assign(next: Me): void {
  Object.assign(me, next);
}

/** Refresh identity from the server. Returns whether the caller is authenticated.
 *  Never throws — a transport error reads as unauthenticated. */
export async function loadMe(): Promise<boolean> {
  try {
    assign(await api.getMe());
  } catch {
    assign({ ...EMPTY });
  }
  return me.authenticated;
}

/** Log out: drop the server session, then re-read identity from the server.
 *  Re-reading (rather than resetting to EMPTY) keeps the login screen's
 *  sign-in methods live — an empty reset pins `methods.github` to false and
 *  hides the GitHub button until the next refresh. */
export async function doLogout(): Promise<void> {
  try {
    await api.logout();
  } catch {
    /* the fallback below still refreshes identity even if the call fails */
  }
  await loadMe();
}
