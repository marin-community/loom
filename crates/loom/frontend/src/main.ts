import { createApp } from 'vue';
import { createRouter, createWebHistory } from 'vue-router';
import App from './App.vue';
import SessionList from './views/SessionList.vue';
import SessionDetail from './views/SessionDetail.vue';
import FileBrowser from './views/FileBrowser.vue';
import Settings from './views/Settings.vue';
import Overlookers from './views/Overlookers.vue';
import OverlookerDetail from './views/OverlookerDetail.vue';
import './styles.css';

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', component: SessionList },
    { path: '/s/:id', component: SessionDetail, props: true },
    { path: '/s/:id/files', component: FileBrowser, props: true },
    { path: '/overlookers', component: Overlookers },
    { path: '/overlookers/:id', component: OverlookerDetail, props: true },
    { path: '/settings', component: Settings },
  ],
});

createApp(App).use(router).mount('#app');
