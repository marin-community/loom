// Bundled fonts (self-hosted woff2 — no CDN; see docs/loom-ui.md). The
// variable sans covers all UI weights in one file; the mono ships the three
// weights the UI uses.
import '@fontsource-variable/ibm-plex-sans';
import '@fontsource-variable/ibm-plex-sans/wght-italic.css';
import '@fontsource/ibm-plex-mono/400.css';
import '@fontsource/ibm-plex-mono/500.css';
import '@fontsource/ibm-plex-mono/600.css';
import { createApp } from 'vue';
import { createRouter, createWebHistory } from 'vue-router';
import App from './App.vue';
import SessionList from './views/SessionList.vue';
import SessionDetail from './views/SessionDetail.vue';
import FileBrowser from './views/FileBrowser.vue';
import Artifacts from './views/Artifacts.vue';
import Settings from './views/Settings.vue';
import Issues from './views/Issues.vue';
import Overlookers from './views/Overlookers.vue';
import OverlookerDetail from './views/OverlookerDetail.vue';
import './styles.css';

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', component: SessionList },
    { path: '/s/:id', component: SessionDetail, props: true },
    { path: '/s/:id/files', component: FileBrowser, props: true },
    { path: '/s/:id/artifacts', component: Artifacts, props: true },
    { path: '/s/:id/artifacts/:name', component: Artifacts, props: true },
    { path: '/issues', component: Issues },
    { path: '/overlookers', component: Overlookers },
    { path: '/overlookers/:id', component: OverlookerDetail, props: true },
    { path: '/settings', component: Settings },
  ],
});

createApp(App).use(router).mount('#app');
