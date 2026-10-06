import { createRouter, createWebHistory } from 'vue-router'
import { ownerPage, state } from './api'

export function workspaceRouter(base = '/') {
  const router = createRouter({
    history: createWebHistory(base),
    routes: [
      { path: '/', component: () => import('./views/Fil.vue') },
      { path: '/atelier', component: () => import('./views/Atelier.vue') },
      { path: '/chats/:id?', component: () => import('./views/Chats.vue') },
      { path: '/mcps', component: () => import('./views/Mcps.vue') },
      { path: '/mcps/callback', component: () => import('./views/McpCallback.vue') },
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
  router.beforeEach(to => state.installationRole === 'member' && ownerPage(to.path) ? '/atelier' : true)
  return router
}
