import type { Component } from 'vue'
import type { ChatWakeup } from '../../../packages/contracts/chats'
import type { ActivityWait } from '../src/activity'
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

const server = await createServer({
  configFile: false,
  plugins: [vue()],
  // This SSR fixture does not need client dependency discovery or prebundling.
  optimizeDeps: { noDiscovery: true },
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
})
let card: Component
beforeAll(async () => {
  card = (await server.ssrLoadModule('/apps/web/src/components/ChatWaitCard.vue')).default
}, 30_000)
afterAll(() => server.close())

const wakeup: ChatWakeup = {
  id: 'wait',
  messageId: 'message',
  reason: 'Wait for checks',
  seconds: 60,
  createdAt: 1,
  wakeAt: 60001,
  condition: {
    type: 'github_pr_checks',
    projectId: 'project',
    prNumber: 1,
    headSha: 'head',
  },
}
const observation = { state: 'completed', type: 'github_pr_checks', conclusion: 'success' }
const resolved: ActivityWait = { wakeup, observation }

function render(wait: ActivityWait, live: { wakeup: ChatWakeup | null, paused: boolean }) {
  return renderToString(createSSRApp({ render: () => h(card, { wait, live }) }))
}

it('an observed result only promises resumption while the wakeup is active', async () => {
  expect(await render(resolved, { wakeup, paused: false })).toContain('Resuming when capacity allows')
  const paused = await render(resolved, { wakeup, paused: true })
  expect(paused).toContain('Wait paused')
  expect(paused).not.toContain('Resuming when capacity allows')
  const cancelled = await render(resolved, { wakeup: null, paused: false })
  expect(cancelled).toContain('Wait cancelled')
  expect(cancelled).not.toContain('Resuming when capacity allows')
  expect(await render({ ...resolved, ended: 'message' }, { wakeup: null, paused: false })).toContain('Wait cancelled by your message')
  expect(await render({ ...resolved, ended: 'replaced' }, { wakeup: null, paused: false })).toContain('Replaced by a new wait')
})

it('completed resumptions and immediate results retain their historical outcome', async () => {
  const resumed = await render({ ...resolved, resumedAt: 1000 }, { wakeup: null, paused: true })
  expect(resumed).toContain('Required checks passed')
  expect(resumed).not.toContain('Wait cancelled')
  expect(await render({ wakeup: null, observation }, { wakeup: null, paused: false })).toContain('The result was already available')
})
