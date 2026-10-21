import type { Component } from 'vue'
import type { ChatWakeup } from '../../../packages/contracts/chats'
import type { ActivityWait } from '../src/activity'
import type { LiveWait } from '../src/chat-wait'
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
    pluginId: 'github',
    projectId: 'project',
    prNumber: 1,
    headSha: 'head',
  },
  display: { title: 'Waiting for required checks on PR #1', short: 'PR #1 checks', details: ['head'] },
}
const observation = {
  state: 'completed',
  type: 'github_pr_checks',
  pluginId: 'github',
  display: { tone: 'ok', title: 'Required checks passed', detail: '' },
}
const resolved: ActivityWait = { wakeup, observation }

function render(wait: ActivityWait, live: Omit<LiveWait, 'settled'>) {
  return renderToString(createSSRApp({ render: () => h(card, { wait, live: { settled: true, ...live } }) }))
}

it('an observed result only promises resumption while the wakeup is active', async () => {
  expect(await render(resolved, { wakeup, paused: false })).toContain('Resuming when capacity allows')
  const paused = await render(resolved, { wakeup, paused: true })
  expect(paused).toContain('Resumes when you resume the conversation')
  expect(paused).not.toContain('Resuming when capacity allows')
  const cancelled = await render(resolved, { wakeup: null, paused: false })
  expect(cancelled).toContain('Wait cancelled')
  expect(cancelled).not.toContain('Resuming when capacity allows')
  expect(await render({ ...resolved, ended: 'cancelled' }, { wakeup: null, paused: false })).toContain('Wait cancelled')
  expect(await render({ ...resolved, ended: 'replaced' }, { wakeup: null, paused: false })).toContain('Replaced by a later wait call')
})

it('completed resumptions and immediate results retain their historical outcome', async () => {
  const resumed = await render({ ...resolved, resumedAt: 1000 }, { wakeup: null, paused: true })
  expect(resumed).toContain('Required checks passed')
  expect(resumed).not.toContain('Wait cancelled')
  expect(await render({ wakeup: null, observation }, { wakeup: null, paused: false })).toContain('The result was already available')
})

it('names a plugin condition with its latest words, and falls back without them', async () => {
  const polled = { ...wakeup, display: { title: 'Waiting for 2 required checks on PR #1', short: 'PR #1 checks', details: ['head'] } }
  const live = await render({ wakeup }, { wakeup: polled, paused: false })
  expect(live).toContain('Waiting for 2 required checks on PR #1')
  expect(live).toContain('head')
  const bare = { ...wakeup, display: null }
  expect(await render({ wakeup: bare }, { wakeup: bare, paused: false })).toContain('Waiting for a condition')
  expect(await render({ wakeup: null, observation: { state: 'completed', type: 'custom', pluginId: 'ci' } }, { wakeup: null, paused: false })).toContain('A condition')
})
