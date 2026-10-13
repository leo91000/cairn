import { createApp } from 'vue'
import { installationRoute } from './api'
import { installationHost } from './beacon-location'
import BeaconApp from './BeaconApp.vue'
import { prepareDraftTransfer } from './draft-transfer'
import { workspaceRouter } from './router'
import DraftTransfer from './views/DraftTransfer.vue'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

prepareDraftTransfer()

const base = installationRoute && !installationHost ? `/installations/${installationRoute}/` : '/'
const router = workspaceRouter(base)

// The beacon account shell renders these pages outside the workspace.
if (!installationRoute) {
  router.addRoute({ path: '/claim', component: { render: () => null } })
  router.addRoute({ path: '/privacy', component: { render: () => null } })
}

if (window.location.pathname === '/browser/drafts' && installationHost) {
  createApp(DraftTransfer).mount('#app')
}
else {
  const app = createApp(BeaconApp).use(router)
  void router.isReady().then(() => app.mount('#app'))
}
