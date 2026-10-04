import { createApp } from 'vue'
import App from './App.vue'
import { workspaceRouter } from './router'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import './styles/index.css'

const router = workspaceRouter()
createApp(App).use(router).mount('#app')
