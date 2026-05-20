import { createApp } from 'vue';
import { createRouter, createWebHashHistory } from 'vue-router';
import App from './App.vue';
import WorkspaceList from './views/WorkspaceList.vue';
import WorkspaceDetail from './views/WorkspaceDetail.vue';
import Settings from './views/Settings.vue';
import './styles.css';

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', component: WorkspaceList },
    { path: '/w/:id', component: WorkspaceDetail, props: true },
    { path: '/settings', component: Settings },
  ],
});

createApp(App).use(router).mount('#app');
