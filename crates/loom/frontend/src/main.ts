import { createApp } from 'vue';
import { createRouter, createWebHashHistory } from 'vue-router';
import App from './App.vue';
import SessionList from './views/SessionList.vue';
import SessionDetail from './views/SessionDetail.vue';
import FileBrowser from './views/FileBrowser.vue';
import Settings from './views/Settings.vue';
import './styles.css';

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', component: SessionList },
    { path: '/s/:id', component: SessionDetail, props: true },
    { path: '/s/:id/files', component: FileBrowser, props: true },
    { path: '/settings', component: Settings },
  ],
});

createApp(App).use(router).mount('#app');
