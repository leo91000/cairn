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
import { createMemoryHistory, createRouter } from 'vue-router'
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
  const app = createSSRApp({ render: () => h(card, { wait, live: { settled: true, ...live } }) })
  // Grouped runs link to their pages.
  app.use(createRouter({ history: createMemoryHistory(), routes: [{ path: '/:path(.*)*', component: { render: () => null } }] }))
  return renderToString(app)
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

it('lists the conditions of a group with their results as they arrive', async () => {
  const runId = '11111111-2222-4333-8444-555555555555'
  const group: ChatWakeup = {
    ...wakeup,
    wakeAt: Date.now() + 600_000,
    condition: { type: 'group', mode: 'all', conditions: [{ type: 'run_finished', runId }, wakeup.condition as never] },
    display: undefined,
    members: [
      {
        observation: {
          state: 'completed',
          type: 'run_finished',
          runId,
          status: 'failed',
        },
      },
      { display: wakeup.display, nextCheckAt: Date.now() + 20_000 },
    ],
  }
  const live = await render({ wakeup: group }, { wakeup: group, paused: false })
  expect(live).toContain('Waiting for 2 results')
  expect(live).toContain('>All<')
  expect(live).toContain('1/2')
  expect(live).toContain('The run failed')
  expect(live).toContain('PR #1 checks')
  expect(live).toContain('Next check in 20 s')
  expect(live).toContain(`/runs/${runId}`)

  const first = {
    state: 'completed',
    type: 'group',
    mode: 'any',
    members: [
      { condition: { type: 'run_finished', runId }, result: null },
      { condition: wakeup.condition, result: observation },
    ],
  }
  const any = { ...group, condition: { ...group.condition!, mode: 'any' } } as ChatWakeup
  const resumed = await render({ wakeup: any, observation: first, resumedAt: 2000 }, { wakeup: null, paused: false })
  expect(resumed).toContain('First result · Required checks passed')
  expect(resumed).toContain('>Any<')
  // The conversation moved on: the list folds behind its meter.
  expect(resumed).toContain('aria-expanded="false"')
  expect(resumed).not.toContain('data-testid="wait-member"')
})

it('identifies delegated tasks, links their conversations and shows task failures', async () => {
  const chatId = 'a11ce123-2222-4333-8444-555555555555'
  const otherId = 'b11ce123-2222-4333-8444-555555555555'
  const group: ChatWakeup = {
    ...wakeup,
    wakeAt: Date.now() + 600_000,
    condition: {
      type: 'group',
      mode: 'all',
      conditions: [{ type: 'task_finished', chatId }, { type: 'task_finished', chatId: otherId }],
    },
    members: [{
      observation: {
        state: 'completed',
        type: 'task_finished',
        status: 'failed',
        error: 'Review failed',
      },
    }, {}],
  }
  const html = await render({ wakeup: group }, { wakeup: group, paused: false })
  expect(html).toContain('Delegated task')
  expect(html).toContain('a11ce123')
  expect(html).toContain('b11ce123')
  expect(html).toContain(`/chats/${chatId}`)
  expect(html).toContain(`/chats/${otherId}`)
  expect(html).toContain('The task failed · Review failed')
  expect(html).toContain('text-danger')
})

it('large groups render a bounded list and retain the full result count', async () => {
  const conditions = Array.from({ length: 100 }, (_, index) => ({ type: 'run_finished' as const, runId: `run-${index}` }))
  const group: ChatWakeup = {
    ...wakeup,
    condition: { type: 'group', mode: 'all', conditions },
    display: undefined,
    members: conditions.map((condition, index) => index === 99 ? { observation: { ...condition, state: 'completed', status: 'failed' } } : {}),
  }
  const html = await render({ wakeup: group }, { wakeup: group, paused: false })
  expect(html.match(/data-testid="wait-member"/g)).toHaveLength(10)
  expect(html).toContain('1/100')
  expect(html).toContain('1–10 of 100')
  expect(html).toContain('Next conditions')
  expect(html).toContain('aria-setsize="100"')
  expect(html).not.toContain('/runs/run-99')
})
