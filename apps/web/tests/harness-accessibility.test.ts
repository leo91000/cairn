import type { Component } from 'vue'
import vue from '@vitejs/plugin-vue'
import { createServer } from 'vite'
import {
  afterAll,
  beforeAll,
  expect,
  it,
} from 'vitest'
import { createSSRApp, h } from 'vue'
import { renderToString } from 'vue/server-renderer'
import { providers } from '../../../packages/contracts/accounts'

const server = await createServer({
  configFile: false,
  plugins: [vue()],
  optimizeDeps: { noDiscovery: true },
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
})
let avatar: Component
let mark: Component
beforeAll(async () => {
  avatar = (await server.ssrLoadModule('/apps/web/src/components/AgentAvatar.vue')).default
  mark = (await server.ssrLoadModule('/apps/web/src/components/HarnessMark.vue')).default
}, 30_000)
afterAll(() => server.close())

it('names the latest harness on header avatars and feed marks', async () => {
  for (const [provider, { label }] of Object.entries(providers)) {
    for (const [component, props] of [[avatar, { name: 'Agent', harness: provider }], [mark, { provider }]] as const) {
      const html = await renderToString(createSSRApp({ render: () => h(component, props) }))
      const root = html.slice(0, html.indexOf('>'))
      expect(root).toContain('role="img"')
      expect(root).toContain(`aria-label="Latest harness: ${label}"`)
      expect(root).not.toContain('aria-hidden="true"')
    }
  }
})

it('keeps avatars without a harness decorative', async () => {
  const html = await renderToString(createSSRApp({ render: () => h(avatar, { name: 'Agent' }) }))
  const root = html.slice(0, html.indexOf('>'))
  expect(root).toContain('aria-hidden="true"')
  expect(root).not.toContain('aria-label=')
})
