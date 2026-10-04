import { createApp } from 'vue'
import { state } from './api'
import OfficialApp from './OfficialApp.vue'
import { workspaceRouter } from './router'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

const base = state.installationId ? `/installations/${state.installationId}/` : '/'
const router = workspaceRouter(base)

const app = createApp(OfficialApp).use(router)
void router.isReady().then(() => app.mount('#app'))
