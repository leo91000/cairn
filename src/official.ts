import { createApp } from 'vue'
import { state } from './api'
import { workspaceRouter } from './router'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

const installationPath = /^\/installations\/([\w-]+)(?:\/|$)/.exec(window.location.pathname)
state.installationId = installationPath?.[1] || ''
const base = state.installationId ? `/installations/${state.installationId}/` : '/'

void import('./OfficialApp.vue').then(({ default: OfficialApp }) => {
  createApp(OfficialApp).use(workspaceRouter(base)).mount('#app')
})
