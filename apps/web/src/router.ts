import { createRouter, createWebHistory } from 'vue-router'
import {
  installationRoute,
  ownerPage,
  redirect,
  state,
} from './api'
import {
  accountSettingsUrl,
  centralAccount,
  installationSubdomains,
  installationUrl,
} from './beacon-location'

export function workspaceRouter(base = '/') {
  const router = createRouter({
    history: createWebHistory(base),
    routes: [
      { path: '/', component: () => import('./views/Fil.vue') },
      { path: '/fil/runs/:id', component: () => import('./views/Fil.vue') },
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
      { path: '/settings/:section(account|installation|sensitive)?', component: () => import('./views/Settings.vue') },
      { path: '/authorize', component: () => import('./views/Authorize.vue') },
      { path: '/:pathMatch(.*)*', redirect: '/' },
    ],
  })
  router.beforeEach((to) => {
    if (installationSubdomains && installationRoute && state.ready && state.authenticated) {
      if (!centralAccount && to.path === '/settings/account') {
        redirect(accountSettingsUrl(installationRoute))
        return false
      }

      if (centralAccount && to.path !== '/settings/account') {
        redirect(installationUrl(installationRoute, to.fullPath))
        return false
      }
    }

    return state.installationRole === 'member' && ownerPage(to.path) ? '/atelier' : true
  })
  return router
}
