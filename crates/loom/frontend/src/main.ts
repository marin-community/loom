// Bundled fonts (self-hosted woff2 — no CDN; see docs/loom-ui.md). Three
// voices: a literary serif for human prose (goals, titles, conversation), a
// quiet humanist sans for UI chrome, and the mono for machine text. Source
// Serif carries an optical-size axis so a 19px lead and an 11px caption are
// each cut for their size. All variable — one file per family covers every
// weight; italics ship separately for the annotation voice.
import '@fontsource-variable/source-serif-4/opsz.css';
import '@fontsource-variable/source-serif-4/opsz-italic.css';
import '@fontsource-variable/source-sans-3/wght.css';
import '@fontsource-variable/source-sans-3/wght-italic.css';
import '@fontsource/ibm-plex-mono/400.css';
import '@fontsource/ibm-plex-mono/500.css';
import '@fontsource/ibm-plex-mono/600.css';
// A second bundled mono, offered as a terminal typeface in Appearance settings
// (the platform stack is the third choice, needing no bundled face).
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import { createApp } from 'vue';
import { createRouter, createWebHistory } from 'vue-router';
import App from './App.vue';
import SessionList from './views/SessionList.vue';
import SessionDetail from './views/SessionDetail.vue';
import Settings from './views/Settings.vue';
import Issues from './views/Issues.vue';
import Watches from './views/Watches.vue';
import Chat from './views/Chat.vue';
import Shell from './views/Shell.vue';
import Login from './views/Login.vue';
import { me, loadMe } from './auth';
import { setUnauthorizedHandler } from './api';
import './styles.css';

const router = createRouter({
  history: createWebHistory(),
  // `meta.title` is the tab-title section for a route ("Weaver - <title>",
  // composed centrally in App.vue). The `/s/:id…` pages intentionally carry none
  // — their section is the live session name, resolved from the fleet snapshot.
  routes: [
    { path: '/login', component: Login, meta: { public: true, title: 'Login' } },
    { path: '/', component: SessionList, meta: { title: 'Sessions' } },
    { path: '/s/:id', component: SessionDetail, props: true },
    // The old Files browser is gone — the embedded editor (a side panel on the
    // detail page) is the file surface now. Redirect stale links there.
    { path: '/s/:id/files', redirect: (to) => `/s/${to.params.id}` },
    // Artifacts is a tab *within* the session page (a kept-alive panel that can
    // pop out beside the terminal), not a page of its own — so these resolve to
    // the same SessionDetail instance and stay deep-linkable.
    { path: '/s/:id/artifacts', component: SessionDetail, props: true },
    { path: '/s/:id/artifacts/:name', component: SessionDetail, props: true },
    { path: '/issues', component: Issues, meta: { title: 'Issues' } },
    { path: '/chat', component: Chat, meta: { title: 'Chat' } },
    { path: '/watches', component: Watches, meta: { title: 'Watches' } },
    { path: '/watches/:id', component: Watches, props: true, meta: { title: 'Watches' } },
    { path: '/shell', component: Shell, meta: { title: 'Shell' } },
    { path: '/settings', component: Settings, meta: { title: 'Settings' } },
  ],
});

// Gate every non-public route on an authenticated identity. A loopback-trusted
// user resolves immediately; anyone else is bounced to the login screen.
router.beforeEach(async (to) => {
  if (to.meta.public) return true;
  if (me.authenticated) return true;
  if (await loadMe()) return true;
  return { path: '/login', query: to.fullPath === '/' ? {} : { redirect: to.fullPath } };
});

// A 401 mid-session (an expired cookie) flips us back to the login screen.
setUnauthorizedHandler(() => {
  me.authenticated = false;
  if (router.currentRoute.value.path !== '/login') {
    router.push({ path: '/login' });
  }
});

// Resolve identity once up front so the first paint picks the right chrome
// (full shell vs. bare login), then mount.
loadMe().finally(() => {
  createApp(App).use(router).mount('#app');
});
