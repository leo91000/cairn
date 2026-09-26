import { createApp } from 'vue'
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', component: () => import('./views/Fil.vue') },
    { path: '/atelier', component: () => import('./views/Atelier.vue') },
    { path: '/chats/:id?', component: () => import('./views/Chats.vue') },
    { path: '/mcps', component: () => import('./views/Mcps.vue') },
    { path: '/tasks', component: () => import('./views/Tasks.vue') },
    { path: '/runs', component: () => import('./views/Runs.vue') },
    { path: '/runs/:id', component: () => import('./views/RunDetail.vue') },
    {
      path: '/agents',
      component: () => import('./views/Resources.vue'),
      props: { kind: 'agents' },
    },
    {
      path: '/projects',
      component: () => import('./views/Resources.vue'),
      props: { kind: 'projects' },
    },
    { path: '/skills', component: () => import('./views/Skills.vue') },
    {
      path: '/connections',
      component: () => import('./views/Connections.vue'),
    },
    { path: '/nodes', component: () => import('./views/Nodes.vue') },
    { path: '/settings', component: () => import('./views/Settings.vue') },
    { path: '/authorize', component: () => import('./views/Authorize.vue') },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})
createApp(App).use(router).mount('#app')
