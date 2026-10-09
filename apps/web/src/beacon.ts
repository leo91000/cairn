import { createApp } from 'vue'
import { state } from './api'
import BeaconApp from './BeaconApp.vue'
import { workspaceRouter } from './router'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

const base = state.installationId ? `/installations/${state.installationId}/` : '/'
const router = workspaceRouter(base)

// The beacon account shell renders these pages outside the workspace.
if (!state.installationId) {
  router.addRoute({ path: '/claim', component: { render: () => null } })
  router.addRoute({ path: '/privacy', component: { render: () => null } })
}

const app = createApp(BeaconApp).use(router)
void router.isReady().then(() => app.mount('#app'))
