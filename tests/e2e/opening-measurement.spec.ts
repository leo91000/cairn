import type { Page } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import process from 'node:process'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

// Opt-in: CAIRN_MEASURE_OPENING=1 pnpm exec playwright test --project=measure-opening
// Records what a reader sees in the frames after opening a conversation from the Fil.
// See docs/CONVERSATION-OPENING.md for the workload, the reference and the results.
test.skip(!process.env.CAIRN_MEASURE_OPENING, 'Opt-in measurement')

interface Frame {
  at: number
  path: string
  messages: number
  previous: boolean
  target: boolean
  loader: boolean
  history: boolean
  anchor: string
  anchorTop: number
}

async function open(page: Page, key: string, previous: string, target: string) {
  const requests: string[] = []
  const collect = (request: { url: () => string }) => {
    if (/\/(?:history|events)\?/.test(request.url()))
      requests.push(request.url())
  }

  page.on('request', collect)
  await page.evaluate(({ previous, target }) => {
    const w = window as any
    w.__frames = []
    w.__shift = 0
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as any[]) {
        if (!entry.hadRecentInput)
          w.__shift += entry.value
      }
    }).observe({ type: 'layout-shift', buffered: false })
    const start = performance.now()

    // A task queued during a frame runs once that frame is painted, after its resize
    // observers: each sample is what the reader saw, not an intermediate layout.
    function afterPaint() {
      setTimeout(sample)
    }

    function sample() {
      const scroller = document.querySelector('[aria-label="Activity output"]')
      const edge = scroller?.getBoundingClientRect().bottom ?? 0
      const messages = [...document.querySelectorAll('.activity-message')]
      const texts = messages.map(message => message.textContent ?? '')
      // The last message the reader sees: where a jump shows first.
      const anchor = messages.findLast(message => message.getBoundingClientRect().top < edge)
      const statuses = [...document.querySelectorAll('[role="status"]')].map(status => status.textContent ?? '')
      w.__frames.push({
        at: performance.now() - start,
        path: location.pathname,
        messages: texts.length,
        previous: texts.some(text => text.includes(previous)),
        target: texts.some(text => text.includes(target)),
        loader: statuses.some(text => /Loading conversation|Opening conversation/.test(text)),
        history: statuses.some(text => text.includes('Loading history')),
        anchor: anchor?.textContent?.slice(0, 80) ?? '',
        anchorTop: Math.round(anchor?.getBoundingClientRect().top ?? 0),
      })
      if (performance.now() - start < 2500)
        requestAnimationFrame(afterPaint)
    }

    requestAnimationFrame(afterPaint)
  }, { previous, target })
  await page.locator(`[data-fil-key="${key}"]`).first().click()
  await page.waitForFunction(() => (window as any).__frames.at(-1).at >= 2500)
  page.off('request', collect)
  const frames: Frame[] = await page.evaluate(() => (window as any).__frames)
  const shift: number = await page.evaluate(() => (window as any).__shift)
  const destination = frames.at(-1)!.path
  const selected = frames.filter(frame => frame.path === destination)
  const shown = selected.findIndex(frame => frame.target)
  let moves = 0
  for (let index = Math.max(shown, 0) + 1; index < selected.length; index++) {
    const before = selected[index - 1]!
    const after = selected[index]!
    if (before.anchor !== after.anchor || Math.abs(before.anchorTop - after.anchorTop) > 1)
      moves++
  }

  return {
    firstContentMs: shown < 0 ? null : Math.round(selected[shown]!.at),
    previousFrames: selected.filter(frame => frame.previous).length,
    // The selected conversation, but without the output the reader came for.
    outdatedFrames: selected.filter(frame => frame.messages && !frame.previous && !frame.target).length,
    loaderFrames: selected.filter(frame => frame.loader).length,
    historyBannerFrames: selected.filter(frame => frame.history).length,
    movesAfterContent: moves,
    layoutShift: Math.round(shift * 1000) / 1000,
    historyRequests: requests.length,
  }
}

// A relayed installation answers after a network round trip; locally everything would
// arrive within a frame or two and hide what a reader sees.
const LATENCY = 150

test('measure conversation opening', async ({ page, workspace }) => {
  test.setTimeout(240000)
  await useRelayForHttpMocks(page)
  await page.route(/\/api\/(?:chats|runs)\/[^/]+\/(?:stream|history|events)\?/, async (route) => {
    await new Promise(resolve => setTimeout(resolve, LATENCY))
    await route.continue()
  })
  await page.setViewportSize({ width: 1440, height: 900 })
  const results: Record<string, unknown>[] = []
  for (let round = 0; round < 3; round++) {
    const first = await workspace.api('/api/chats', 'POST', {})
    const second = await workspace.api('/api/chats', 'POST', {})
    const firstText = `Round ${round} first conversation`
    const secondText = `Round ${round} second conversation`
    for (const [chat, text] of [[first, firstText], [second, secondText]] as const) {
      await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
      await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
    }

    // The second conversation has a long history: tool calls and answers in many turns.
    const run = workspace.service.chats.detail(second.id).run!
    for (let turn = 0; turn < 30; turn++) {
      workspace.service.store.event(run.id, 'chat.user', `Question ${turn}`, { messageId: randomUUID(), text: `Question ${turn}` })
      for (let call = 0; call < 15; call++) {
        workspace.service.store.event(run.id, 'item.completed', 'pnpm test', {
          item: {
            id: `call-${turn}-${call}`,
            type: 'command_execution',
            command: 'pnpm test',
            aggregated_output: 'ok\n'.repeat(20),
            exit_code: 0,
          },
        })
      }

      const answer = `Answer ${turn}. ${'A paragraph of the agent reply that wraps over several lines. '.repeat(6)}`
      workspace.service.store.event(run.id, 'item.completed', answer, { item: { id: `answer-${turn}`, type: 'agent_message', text: answer } })
    }

    const latest = `Round ${round} latest answer`
    workspace.service.store.event(run.id, 'item.completed', latest, { item: { id: 'latest', type: 'agent_message', text: latest } })

    // A third conversation ends on hundreds of tool calls: its recent window folds into a
    // few rows, too short to fill the view.
    const tools = await workspace.api('/api/chats', 'POST', {})
    const toolsText = `Round ${round} tool conversation`
    await workspace.api(`/api/chats/${tools.id}/messages`, 'POST', { id: randomUUID(), text: toolsText })
    await expect.poll(() => workspace.service.chats.detail(tools.id).run?.status).toBe('succeeded')
    const toolsRun = workspace.service.chats.detail(tools.id).run!
    for (let call = 0; call < 320; call++) {
      workspace.service.store.event(toolsRun.id, 'item.completed', 'cat README.md', {
        item: {
          id: `read-${call}`,
          type: 'command_execution',
          command: 'cat README.md',
          exit_code: 0,
        },
      })
    }

    const toolsLatest = `Round ${round} after the tools`
    workspace.service.store.event(toolsRun.id, 'item.completed', toolsLatest, { item: { id: 'tools-latest', type: 'agent_message', text: toolsLatest } })

    await page.goto(workspacePath(`/chats/${first.id}`))
    await authenticateWorkspace(page)
    await expect(page.locator('.activity-message').getByText(firstText, { exact: true }).first()).toBeVisible()
    await page.waitForTimeout(500)

    results.push({ round, scenario: 'first open', ...await open(page, `chat:${second.id}`, firstText, latest) })
    await page.waitForTimeout(500)
    results.push({ round, scenario: 'back to a saved conversation', ...await open(page, `chat:${first.id}`, latest, firstText) })
    await page.waitForTimeout(500)
    results.push({ round, scenario: 'saved long conversation', ...await open(page, `chat:${second.id}`, firstText, latest) })
    await page.waitForTimeout(500)
    results.push({ round, scenario: 'first open of folded tool calls', ...await open(page, `chat:${tools.id}`, latest, toolsLatest) })
    await page.waitForTimeout(500)
    await page.locator(`[data-fil-key="chat:${first.id}"]`).first().click()
    await expect(page.locator('.activity-message').getByText(firstText, { exact: true }).first()).toBeVisible()

    // The long conversation finishes another turn while the reader is elsewhere.
    const later = `Round ${round} later question`
    await workspace.api(`/api/chats/${second.id}/messages`, 'POST', { id: randomUUID(), text: later })
    await expect.poll(() => workspace.service.chats.detail(second.id).messages.some(message => message.text === later && message.status === 'delivered')).toBe(true)
    await expect.poll(() => workspace.service.chats.detail(second.id).run?.status).toBe('succeeded')
    await page.waitForTimeout(1000)
    results.push({ round, scenario: 'outdated saved conversation', ...await open(page, `chat:${second.id}`, firstText, later) })
  }

  const scenarios = [...new Set(results.map(result => result.scenario as string))]
  const median = (values: number[]) => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)]!
  const summary = Object.fromEntries(scenarios.map((scenario) => {
    const rows = results.filter(result => result.scenario === scenario)
    const keys = Object.keys(rows[0]!).filter(key => key !== 'round' && key !== 'scenario')
    return [scenario, Object.fromEntries(keys.map(key => [key, median(rows.map(row => Number(row[key] ?? Number.NaN)))]))]
  }))
  await test.info().attach('opening.json', { body: JSON.stringify({ results, summary }, null, 2), contentType: 'application/json' })
  process.stdout.write(`OPENING_PROFILE ${JSON.stringify({ results, summary })}\n`)
})
