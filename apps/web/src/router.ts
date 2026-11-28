import { createRouter, createWebHistory } from 'vue-router'
import {
  installationRoute,
  ownerPage,
  redirect,
  state,
} from './api'
import {
  accountSettingsUrl,
  beaconOrigin,
  centralAccount,
  installationSubdomains,
  installationUrl,
} from './beacon-location'

export function workspaceRouter(base = '/') {
  const router = createRouter({
    history: createWebHistory(base),
    routes: [
      { path: '/', component: () => import('./views/Fil.vue') },
      { path: '/atelier', component: () => import('./views/Atelier.vue') },
      { path: '/chats/:id?', component: () => import('./views/Chats.vue') },
      { path: '/plugins', component: () => import('./views/Plugins.vue') },
      { path: '/plugins/oauth/callback', component: () => import('./views/PluginOAuthCallback.vue'), props: { embedded: true } },
      { path: '/plugins/:id', component: () => import('./views/PluginGeneric.vue') },
      { path: '/mcps', component: () => import('./views/Mcps.vue') },
      { path: '/mcps/callback', component: () => import('./views/McpCallback.vue') },
      { path: '/rules', component: () => import('./views/Rules.vue') },
      { path: '/rules/new', component: () => import('./views/RuleEditor.vue') },
      { path: '/rules/:id/edit', component: () => import('./views/RuleEditor.vue') },
      { path: '/runs', component: () => import('./views/Runs.vue') },
      { path: '/runs/:id', component: () => import('./views/RunDetail.vue') },
      {
        path: '/agents',
        component: () => import('./views/Resources.vue'),
        props: { kind: 'agents' },
      },
      { path: '/agents/:id', component: () => import('./views/AgentPage.vue') },
      { path: '/agents/:id/configuration/:provider(codex|claude|opencode|antigravity)', component: () => import('./views/CodingAgentConfiguration.vue') },
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
      { path: '/settings/coding-agents/:provider(codex|claude|opencode|antigravity)', component: () => import('./views/CodingAgentConfiguration.vue') },
      // Consent belongs to the account, not an installation: Beacon's page shows the signed-in
      // account and the installation it last used.
      {
        path: '/authorize',
        component: { render: () => null },
        beforeEnter: (to) => {
          const target = new URL(to.fullPath, beaconOrigin)
          // Beacon's own page renders consent itself; only leave an installation's copy.
          if (target.origin === window.location.origin && target.pathname === window.location.pathname)
            return true
          redirect(target.href)
          return false
        },
      },
      { path: '/:pathMatch(.*)*', redirect: '/' },
    ],
  })
  router.beforeEach((to) => {
    if (to.path === '/' && typeof to.query.rule === 'string')
      return { path: '/rules', query: to.query }

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
