import type { Browser, BrowserContext, Page } from '@playwright/test'
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { readFile, writeFile } from 'node:fs/promises'
import { createConnection } from 'node:net'
import { join, resolve } from 'node:path'
import process from 'node:process'
import { chromium, expect, test } from '@playwright/test'
import { beaconRelayFixture } from './beacon-relay-fixture'
import { captureNetworkTransport, networkTransportDiagnostics } from './network-transport-diagnostics'

interface Proof {
  armed: boolean
  attempts: number
  droppedDatagrams: number
  droppedDataChunks: number
  passedTarget: number
  passedHeaders: number
  droppedHandshake: number
  openingPeers: number
  openingDrops: number[]
  errors: number
  attemptTimesMs: number[]
  dispatch: {
    event: string
    method: string
    elapsedMs: number
    bytes: number | null
  }[]
}

interface Observation {
  route: string
  method: string
  cursor?: number
  at: number
}

interface Harness {
  configure: (installation: string, csrf: string) => void
  route: () => string
  live: (chat: string) => void
  stop: () => void
  stopNegotiation: () => void
  request: (key: string, path: string, method?: string, body?: unknown) => void
  accepted: {
    cursor: number
    reset: boolean
    history?: string
    ids: number[]
    messages: string[]
  }[]
  requests: Record<string, {
    done: boolean
    status?: number
    route?: string
    failed?: boolean
  }>
}

declare global {
  interface Window { ci4: Harness, qualificationObservations: Observation[] }
}

async function control(command: object): Promise<Proof> {
  const socket = createConnection(join(process.env.CAIRN_NETWORK_FIXTURE_DIRECTORY!, 'qualification-control'))
  socket.setTimeout(2000, () => socket.destroy(new Error('qualification control timeout')))
  let result = ''
  socket.on('data', chunk => result += chunk)
  const ended = once(socket, 'end')
  socket.end(`${JSON.stringify(command)}\n`)
  await ended
  return JSON.parse(result)
}

async function arm(mode: string, fragmented = false, nextPeer = true, openingPeers = 1) {
  const proof = await control({
    command: 'arm',
    mode: process.env.CAIRN_QUALIFICATION_NEGATIVE && mode === 'recoverable' ? 'pass' : mode,
    fragmented,
    nextPeer,
    openingPeers,
  })
  expect(proof.armed).toBe(true)
  expect(proof.errors).toBe(0)
  expect(proof.droppedDatagrams).toBe(0)
}

function requireRecoveryProof(proof: Proof) {
  expect(proof.armed, 'drop plan must acknowledge arming').toBe(true)
  expect(proof.errors, 'every application DTLS record must correlate with observed SCTP').toBe(0)
  expect(proof.droppedDatagrams, 'initial DATA and one retransmission must actually be dropped').toBe(2)
  expect(proof.droppedDataChunks).toBeGreaterThanOrEqual(2)
  expect(proof.attempts).toBeGreaterThanOrEqual(3)
  expect(proof.passedTarget).toBeGreaterThanOrEqual(1)
}

test('bounded DATA loss delivers directly and precise starvation recovers safely', async () => {
  test.setTimeout(240000)
  const fixture = await beaconRelayFixture(4398, '198.18.103.1', process.env.CAIRN_NETWORK_FIXTURE_DIRECTORY)
  const started = performance.now()
  const evidence: object[] = []
  const activeRust = new Map<ChildProcess, Promise<unknown>>()
  let browser: Browser | undefined
  let current: Page | undefined
  let context: BrowserContext | undefined
  let installationId = ''
  let csrf = ''
  let chatId = ''
  let cookie = ''
  const seedText = `Bounded qualification payload ${'0123456789abcdef'.repeat(220)}`
  const chats = () => `/api/installations/${installationId}/api/chats`

  async function relay(method: string, suffix = '', body?: object) {
    return context!.request.fetch(`${fixture.url}${chats()}${suffix}`, {
      method,
      headers: { 'origin': fixture.url, 'x-csrf-token': csrf },
      data: body,
    })
  }

  async function document() {
    await current?.close()
    const page = current = await context!.newPage()
    await captureNetworkTransport(page)
    const startupErrors: string[] = []
    page.on('pageerror', error => startupErrors.push(error.name))
    await page.addInitScript(() => {
      window.qualificationObservations = []
      window.addEventListener('cairn-transport-observation', (event) => {
        const detail = (event as CustomEvent).detail
        window.qualificationObservations.push({
          route: detail.route,
          method: detail.method,
          cursor: detail.cursor,
          at: performance.now(),
        })
      })
    })
    await page.route('**/ci4-qualification', route => route.fulfill({
      contentType: 'text/html',
      body: '<!doctype html><div id="app" data-beacon></div><pre id="content"></pre>',
    }))
    await page.goto(`${fixture.url}/installations/${installationId}/ci4-qualification`)
    await page.addScriptTag({ path: resolve('test-results/qualification-bundle/harness.js') })
    expect(startupErrors, 'qualification bundle must execute without an error').toEqual([])
    expect(await page.evaluate(() => typeof window.ci4)).toBe('object')
    return page
  }

  async function configure(page: Page) {
    await page.evaluate(({ installationId, csrf }) => window.ci4.configure(installationId, csrf), { installationId, csrf })
  }

  async function ready(page: Page) {
    await configure(page)
    await expect.poll(() => page.evaluate(() => window.ci4.route()), { timeout: 30000 }).toBe('direct')
  }

  async function live(page: Page, route: string) {
    await page.evaluate(chat => window.ci4.live(chat), chatId)
    await expect.poll(() => page.evaluate(route => window.qualificationObservations.some(item => item.method === 'STREAM' && item.route === route), route), { timeout: 25000 }).toBe(true)
    await expect(page.locator('#content')).toContainText(seedText)
  }

  async function request(page: Page, key: string, path: string, method = 'GET', body?: object) {
    await page.evaluate(({
      key,
      path,
      method,
      body,
    }) => window.ci4.request(key, path, method, body), {
      key,
      path,
      method,
      body,
    })
  }

  async function completed(page: Page, key: string, route: string) {
    await expect.poll(() => page.evaluate(key => window.ci4.requests[key], key), { timeout: 25000 }).toMatchObject({ done: true, status: 200, route })
  }

  async function storedOnce(id: string, text: string) {
    const response = await relay('GET', `/${chatId}`)
    expect(response.status()).toBe(200)
    const detail = await response.json()
    expect(detail.messages.filter((message: { id: string, text: string }) => message.id === id && message.text === text)).toHaveLength(1)
    expect(detail.messages.filter((message: { text: string }) => message.text === text)).toHaveLength(1)
  }

  async function rust(requestLoss = false) {
    const child = spawn(process.env.CAIRN_NETWORK_DIRECT_CLIENT!, [fixture.url, installationId, `/api/chats/${chatId}${requestLoss ? `?ci4Padding=${'a'.repeat(4096)}` : ''}`, seedText, ...(requestLoss ? ['--bounded-request-loss'] : [])], { stdio: ['pipe', 'pipe', 'ignore'] })
    let output = ''
    child.stdout.on('data', chunk => output += chunk)
    const exited = once(child, 'close')
    activeRust.set(child, exited)
    child.stdin.end(cookie)
    try {
      const [status] = await exited
      expect(status, 'authenticated Rust fixture must finish successfully').toBe(0)
      const result = JSON.parse(output)
      evidence.push({
        case: 'rust-observation',
        boundedRequestLoss: requestLoss,
        result,
        nativeProof: await control({ command: 'stats' }),
      })
      return result
    }
    finally {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill('SIGKILL')
        await exited
      }

      activeRust.delete(child)
    }
  }

  try {
    const beacon = fixture.beacon()
    browser = await chromium.launch({ executablePath: process.env.CAIRN_NETWORK_CHROMIUM, args: ['--disable-features=WebRtcHideLocalIpsWithMdns'] })
    context = await browser.newContext()
    const page = current = await context.newPage()
    // Bootstrap is authenticated relay traffic and never direct-delivery proof.
    await page.route('**/direct/authorize', route => route.fulfill({ contentType: 'application/json', body: '{"available":false}' }))
    await expect.poll(() => {
      expect(beacon.exitCode).toBeNull()
      return fetch(`${fixture.url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    await page.goto(fixture.url)
    await page.getByLabel('Email address').fill(`ci4-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => fixture.messages.length).toBe(1)
    await page.getByLabel('Email code').fill(fixture.messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    const installation = fixture.start('target/debug/cairn', {
      DATA_DIR: join(fixture.root, 'data'),
      AGENT_HOME: join(fixture.root, 'home'),
      WORKSPACE_ROOTS: fixture.root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      HOST: '127.0.0.1',
      PORT: '4399',
      CAIRN_BEACON_ORIGIN: fixture.url,
      CAIRN_INSTALLATION_CLAIM_CODE: code,
    })
    await expect(async () => {
      expect(installation.exitCode).toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass({ timeout: 30000 })
    const account = await (await page.request.get(`${fixture.url}/api/account/session`)).json()
    csrf = account.csrf
    installationId = account.installations[0].id
    cookie = (await context.cookies()).map(item => `${item.name}=${item.value}`).join('; ')
    const created = await relay('POST', '', {})
    expect(created.status()).toBe(200)
    chatId = (await created.json()).id
    const seeded = await relay('POST', `/${chatId}/messages`, { id: randomUUID(), text: seedText })
    expect(seeded.status()).toBe(200)
    const seededChat = fixture.seed.store.list('chats').find(chat => chat.id === chatId)!
    const task = fixture.seed.task({ name: 'Synthetic cursor qualification', prompt: 'Fixture only', agentId: seededChat.agentId })
    const run = await fixture.seed.enqueue(task.id)
    fixture.seed.store.updateRun(run.id, { status: 'succeeded', finishedAt: Date.now() })
    fixture.seed.store.put('chats', { ...seededChat, runId: run.id })
    for (const value of [1, 2, 3])
      fixture.seed.store.event(run.id, 'item.completed', `Initial synthetic event ${value}`, {})

    // The exact same first-application starvation plan serves both contracts.
    for (const mode of ['starve', 'headers-only']) {
      const page = await document()
      await arm(mode)
      await ready(page)
      await page.evaluate(chat => window.ci4.live(chat), chatId)
      const id = randomUUID()
      const text = `First application starvation ${mode}`
      await request(page, 'send', `/chats/${chatId}/messages`, 'POST', { id, text })
      await completed(page, 'send', 'relay')
      await expect.poll(() => page.evaluate(() => window.qualificationObservations.some(item => item.method === 'STREAM' && item.route === 'relay')), { timeout: 25000 }).toBe(true)
      await expect(page.locator('#content')).toContainText(text)
      await storedOnce(id, text)
      const diagnostics = await networkTransportDiagnostics(page)
      const heartbeat = diagnostics.find(item => item.kind === 'heartbeat-timeout')!
      const opened = diagnostics.find(item => item.kind === 'channel-open')!
      expect(heartbeat).toBeDefined()
      expect(opened).toBeDefined()
      expect(heartbeat.elapsedMs - opened.elapsedMs).toBeGreaterThanOrEqual(14900)
      expect(heartbeat.elapsedMs - opened.elapsedMs).toBeLessThan(17000)
      const closed = diagnostics.find(item => item.kind === 'channel-close')!
      expect(closed).toBeDefined()
      expect(closed.elapsedMs - heartbeat.elapsedMs).toBeGreaterThanOrEqual(0)
      expect(closed.elapsedMs - heartbeat.elapsedMs).toBeLessThan(1000)
      const observations = await page.evaluate(() => window.qualificationObservations)
      const priorDirect = observations.some(item => item.route === 'direct' && ['GET', 'POST', 'STREAM'].includes(item.method) && item.at < heartbeat.elapsedMs)
      expect(priorDirect).toBe(false)
      const proof = await control({ command: 'stats' })
      expect(proof.errors).toBe(0)
      expect(proof.droppedDataChunks).toBeGreaterThan(0)
      const receivedBeforeHeartbeat = diagnostics.filter(item => item.kind === 'data-channel' && item.elapsedMs < heartbeat.elapsedMs) as (typeof diagnostics[number] & { messagesReceived: number })[]
      if (mode === 'headers-only') {
        expect(proof.passedHeaders).toBeGreaterThan(0)
        expect(receivedBeforeHeartbeat.some(item => item.messagesReceived > 0)).toBe(true)
      }
      else {
        expect(receivedBeforeHeartbeat.length).toBeGreaterThan(0)
        expect(receivedBeforeHeartbeat.every(item => item.messagesReceived === 0)).toBe(true)
      }

      evidence.push({
        case: `first-application-${mode}`,
        proof,
        oldOracle: 'would-fail',
        openedMs: opened.elapsedMs,
        heartbeatMs: heartbeat.elapsedMs,
        closedMs: closed.elapsedMs,
        receivedMessagesBeforeHeartbeat: Math.max(0, ...receivedBeforeHeartbeat.map(item => item.messagesReceived)),
        authenticatedRelayPost: true,
        acceptedRelayBatch: true,
        exactlyOnce: true,
      })
      if (process.env.CAIRN_QUALIFICATION_OLD_ORACLE) {
        // The unchanged line-212 prerequisite, after independently proving rescue.
        expect(priorDirect, 'the browser must have completed authenticated direct traffic before fallback').toBe(true)
      }

      await page.evaluate(() => window.ci4.stop())
    }

    {
      const page = await document()
      await arm('recoverable', true)
      await ready(page)
      await live(page, 'direct')
      await request(page, 'read', `/chats/${chatId}`)
      await completed(page, 'read', 'direct')
      const id = randomUUID()
      const text = 'Browser recovered direct message'
      await request(page, 'send', `/chats/${chatId}/messages`, 'POST', { id, text })
      await completed(page, 'send', 'direct')
      await storedOnce(id, text)
      const proof = await control({ command: 'stats' })
      const observation = {
        case: 'browser-fragmented-response-recoverable',
        proof,
        route: 'direct',
        exactlyOnce: true,
        proofVerified: false,
      }
      evidence.push(observation)
      requireRecoveryProof(proof)
      observation.proofVerified = true
      expect((await networkTransportDiagnostics(page)).some(item => item.kind === 'heartbeat-timeout')).toBe(false)
      await page.evaluate(() => window.ci4.stop())
    }

    // Preserve the stronger prior-completed-direct requirement where guaranteed.
    {
      const page = await document()
      await arm('pass')
      await ready(page)
      await live(page, 'direct')
      await request(page, 'initial-read', `/chats/${chatId}`)
      await completed(page, 'initial-read', 'direct')
      const acceptedBeforeLoss = await page.evaluate(() => window.ci4.accepted.at(-1)!)
      const acceptedCursor = acceptedBeforeLoss.cursor
      expect(acceptedCursor).toBeGreaterThan(0)
      const resumes: { cursor: number, history: string | null }[] = []
      let mutationRelayPosts = 0
      page.on('request', (request) => {
        const url = new URL(request.url())
        if (url.pathname.endsWith(`/chats/${chatId}/stream`))
          resumes.push({ cursor: Number(url.searchParams.get('after')), history: url.searchParams.get('history') })
        if (url.pathname === chats() && request.method() === 'POST')
          mutationRelayPosts++
      })
      const initialChats = (await (await relay('GET')).json()).length
      await arm('starve', false, false)
      for (const value of [4, 5])
        fixture.seed.store.event(run.id, 'item.completed', `Resumed synthetic event ${value}`, {})
      const id = randomUUID()
      const text = 'Established traffic idempotent send'
      await request(page, 'send', `/chats/${chatId}/messages`, 'POST', { id, text })
      await request(page, 'mutation', '/chats', 'POST', {})
      await completed(page, 'send', 'relay')
      await expect.poll(() => page.evaluate(() => window.ci4.requests.mutation), { timeout: 25000 }).toMatchObject({ done: true, failed: true })
      await expect(page.locator('output[data-operation=mutation]')).toHaveText('Request failed')
      await expect.poll(() => page.evaluate(() => window.qualificationObservations.some(item => item.method === 'STREAM' && item.route === 'relay')), { timeout: 25000 }).toBe(true)
      await expect(page.locator('#content')).toContainText(text)
      await storedOnce(id, text)
      expect(mutationRelayPosts).toBe(0)
      expect((await (await relay('GET')).json()).length).toBe(initialChats + 1)
      expect(resumes[0]).toEqual({ cursor: acceptedCursor, history: acceptedBeforeLoss.history })
      const acceptedIds = await page.evaluate(() => window.ci4.accepted.flatMap(batch => batch.ids))
      expect(acceptedIds).toEqual(fixture.seed.store.events(run.id).map(event => event.id))
      expect(new Set(acceptedIds).size).toBe(acceptedIds.length)
      expect(await page.evaluate(id => window.ci4.accepted.at(-1)!.messages.filter(value => value === id).length, id)).toBe(1)
      const observations = await page.evaluate(() => window.qualificationObservations)
      const diagnostics = await networkTransportDiagnostics(page)
      const heartbeat = diagnostics.find(item => item.kind === 'heartbeat-timeout')!
      const closed = diagnostics.find(item => item.kind === 'channel-close')!
      expect(heartbeat).toBeDefined()
      expect(closed).toBeDefined()
      expect(closed.elapsedMs - heartbeat.elapsedMs).toBeGreaterThanOrEqual(0)
      expect(closed.elapsedMs - heartbeat.elapsedMs).toBeLessThan(1000)
      expect(observations.some(item => item.method === 'GET' && item.route === 'direct' && item.at < heartbeat.elapsedMs)).toBe(true)
      expect(observations.some(item => item.method === 'STREAM' && item.route === 'direct' && item.at < heartbeat.elapsedMs)).toBe(true)
      const proof = await control({ command: 'stats' })
      expect(proof.errors).toBe(0)
      expect(proof.droppedDataChunks).toBeGreaterThan(0)
      evidence.push({
        case: 'established-starvation',
        proof,
        heartbeatMs: heartbeat.elapsedMs,
        closedMs: closed.elapsedMs,
        acceptedCursor,
        resumedCursor: resumes[0]!.cursor,
        historyMatched: true,
        acceptedEventCount: acceptedIds.length,
        priorDirectRead: true,
        priorDirectBatch: true,
        authenticatedRelayPost: true,
        cursorResumed: true,
        exactlyOnce: true,
        nonIdempotentReplayRefused: true,
      })
      await page.evaluate(() => window.ci4.stop())
    }

    await current!.close()
    current = undefined

    // A real member keeps this independent group within the unchanged
    // eight-leases-per-account ceiling; no capacity or rate limit is bypassed.
    const memberEmail = `ci4-reader-${Date.now()}@example.test`
    const invited = await context.request.post(`${fixture.url}/api/installations/${installationId}/sharing/invitations`, {
      headers: { 'origin': fixture.url, 'x-csrf-token': csrf },
      data: { email: memberEmail },
    })
    expect(invited.ok()).toBe(true)
    const member = await browser.newPage()
    await member.route('**/direct/authorize', route => route.fulfill({ contentType: 'application/json', body: '{"available":false}' }))
    await member.goto(fixture.url)
    await member.getByLabel('Email address').fill(memberEmail)
    const beforeCode = fixture.messages.length
    await member.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => fixture.messages.slice(beforeCode).find(message => /\b\d{8}\b/.test(message))).toBeTruthy()
    await member.getByLabel('Email code').fill(fixture.messages.slice(beforeCode).find(message => /\b\d{8}\b/.test(message))!.match(/\b\d{8}\b/)![0])
    await member.getByRole('button', { name: 'Sign in', exact: true }).click()
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(/\/installations\/[^/]+\/$/)
    const memberSession = await (await member.request.get(`${fixture.url}/api/account/session`)).json()
    expect(memberSession.installations.find((item: { id: string }) => item.id === installationId).role).toBe('member')
    cookie = (await member.context().cookies()).map(item => `${item.name}=${item.value}`).join('; ')
    await member.close()

    for (let sample = 0; sample < 3; sample++) {
      await arm('pass')
      const result = await rust(true)
      expect(result.route).toBe('direct')
      expect(result.directFailure).toBeNull()
      expect(result.requestLoss).toMatchObject({ armed: true, droppedDatagrams: 2, errors: 0 })
      expect(result.requestLoss.passedTarget).toBeGreaterThan(0)
      evidence.push({ case: 'rust-request-recoverable', sample, result })
      await arm('recoverable', true)
      const response = await rust()
      expect(response.route).toBe('direct')
      expect(response.directFailure).toBeNull()
      const proof = await control({ command: 'stats' })
      requireRecoveryProof(proof)
      evidence.push({
        case: 'rust-fragmented-response-recoverable',
        sample,
        result: response,
        proof,
      })
    }

    await arm('starve')
    const starved = await rust()
    expect(starved.route).toBe('relay')
    expect(starved.directFailure).toEqual({ phase: 'response', timedOut: true })
    expect(starved.candidatePair).toBeNull()
    expect(starved.directPhases.map((phase: { event: string }) => phase.event)).toContain('request-queued')
    expect(starved.directPhases.map((phase: { event: string }) => phase.event)).not.toContain('complete-response')
    expect(starved.directPhases.find((phase: { event: string }) => phase.event === 'channel-open').remainingMs).toBeGreaterThan(20000)
    const starvationProof = await control({ command: 'stats' })
    expect(starvationProof.errors).toBe(0)
    expect(starvationProof.droppedDataChunks).toBeGreaterThan(0)
    expect(starvationProof.attempts).toBeGreaterThanOrEqual(4)
    expect(starved.responseMessages).toBe(0)
    expect(starvationProof.dispatch.some(event => event.event === 'request-dispatch' && event.method === 'GET')).toBe(true)
    expect(starvationProof.dispatch.some(event => event.event === 'response-produced' && event.method === 'GET')).toBe(true)
    evidence.push({ case: 'rust-response-starvation', result: starved, proof: starvationProof })

    const openingPage = await document()
    await arm('opening', false, true, 2)
    await configure(openingPage)
    await live(openingPage, 'relay')
    const duringId = randomUUID()
    const duringText = 'Authenticated relay remains usable during opening'
    await request(openingPage, 'during-opening', `/chats/${chatId}/messages`, 'POST', { id: duringId, text: duringText })
    await completed(openingPage, 'during-opening', 'relay')
    await expect(openingPage.locator('#content')).toContainText(duringText)
    await storedOnce(duringId, duringText)
    const openingAttempt = rust()
    void openingAttempt.catch(() => {})
    await expect.poll(async () => (await networkTransportDiagnostics(openingPage)).some(item => item.kind === 'channel-close'), { timeout: 35000 }).toBe(true)
    const openingDiagnostics = await networkTransportDiagnostics(openingPage)
    const createdPeer = openingDiagnostics.find(item => item.kind === 'created')!
    const closedChannel = openingDiagnostics.find(item => item.kind === 'channel-close')!
    expect(closedChannel.elapsedMs - createdPeer.elapsedMs).toBeGreaterThanOrEqual(29500)
    expect(closedChannel.elapsedMs - createdPeer.elapsedMs).toBeLessThan(32000)
    expect(openingDiagnostics.some(item => item.kind === 'channel-open' || item.kind === 'heartbeat-timeout')).toBe(false)
    // Teardown only after the real opening failure; retain the live relay consumer.
    await openingPage.evaluate(() => window.ci4.stopNegotiation())
    const opening = await openingAttempt
    expect(opening.route).toBe('relay')
    expect(opening.directFailure).toEqual({ phase: 'ice', timedOut: true })
    expect(opening.directPhases.map((phase: { event: string }) => phase.event)).not.toContain('channel-open')
    const afterId = randomUUID()
    const afterText = 'Authenticated relay continues after opening failure'
    await request(openingPage, 'after-opening', `/chats/${chatId}/messages`, 'POST', { id: afterId, text: afterText })
    await completed(openingPage, 'after-opening', 'relay')
    await expect(openingPage.locator('#content')).toContainText(afterText)
    await storedOnce(afterId, afterText)
    expect(await openingPage.evaluate(() => window.qualificationObservations.every(item => item.route === 'relay'))).toBe(true)
    const openingProof = await control({ command: 'stats' })
    expect(openingProof.errors).toBe(0)
    expect(openingProof.openingPeers).toBe(2)
    expect(openingProof.openingDrops.every(count => count > 0)).toBe(true)
    expect(openingProof.dispatch).toHaveLength(0)
    evidence.push({
      case: 'opening-starvation',
      result: opening,
      proof: openingProof,
      browserOpeningMs: closedChannel.elapsedMs - createdPeer.elapsedMs,
      authenticatedRelayPost: true,
      acceptedRelayBatch: true,
      exactlyOnce: true,
    })
  }
  finally {
    for (const [child, exited] of activeRust) {
      if (child.exitCode === null && child.signalCode === null)
        child.kill('SIGKILL')
      await exited.catch(() => {})
    }

    if (current && !current.isClosed())
      await current.evaluate(() => window.ci4?.stop()).catch(() => {})
    await browser?.close()
    await fixture.close()
    const output = process.env.CAIRN_NETWORK_OUTPUT!
    const report = JSON.parse(await readFile(output, 'utf8'))
    await writeFile(output, `${JSON.stringify({ ...report, qualification: evidence, qualificationElapsedMs: performance.now() - started })}\n`)
  }
})
