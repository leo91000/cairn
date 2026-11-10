
import { writeFileSync as saveAvailabilityJournal } from 'node:fs'

// These functions are the real probe orchestration seams. Light controller tests
// exercise them without launching a browser; no synthetic terminal is produced.
export async function skillProbeBound(operation: () => Promise<unknown>, label: string) {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Skill probe deadline: ${label}`)), 7000)
      }),
    ])
  }
  finally {
    clearTimeout(timer)
  }
}

export async function commitSkillReload(control: {
  document: string
  read: number
  held: () => boolean
  snapshot: () => Promise<{ document?: string, pending?: number }>
  terminals: () => Promise<Array<{ kind: string, document?: string, poll?: number }>>
  drain: () => Promise<void>
  advance: () => Promise<void>
}) {
  if (!control.held())
    throw new Error('The original skill poll body is no longer held')
  const before = await skillProbeBound(control.snapshot, 'old document identity') as { document?: string }
  if (!control.document || before.document !== control.document)
    throw new Error('Wrong or unknown old skill document')
  const terminals = await skillProbeBound(control.terminals, 'original old poll terminal') as Array<{ kind: string, document?: string, poll?: number }>
  const terminal = terminals[0]
  if (terminals.length !== 1 || !terminal || terminal.document !== control.document || terminal.poll !== control.read
    || !['poll-rejected', 'poll-threw'].includes(terminal.kind)) {
    throw new Error('Missing, duplicate or wrong original old skill poll terminal')
  }
  await skillProbeBound(control.drain, 'old document JavaScript drain')
  const after = await skillProbeBound(control.snapshot, 'old document closure') as { document?: string, pending?: number }
  const final = await skillProbeBound(control.terminals, 'final original poll receipt') as typeof terminals
  if (after.document !== control.document || after.pending !== 0 || final.length !== 1 || final[0] !== terminal || !control.held())
    throw new Error('Old skill document or original poll closure is incomplete')
  await skillProbeBound(control.advance, 'owned reload commit')
}

export async function closeAndAuditSkillProbe(control: {
  drain: () => Promise<void>
  close: Array<() => Promise<unknown>>
  finished: () => Promise<void>
  audit: () => void
}) {
  const failures: unknown[] = []
  try {
    await skillProbeBound(control.drain, 'final JavaScript drain')
  }
  catch (cause) {
    failures.push(cause)
  }
  // Start every owner independently; a stalled navigation must not gate page close.
  const closures = await Promise.allSettled(control.close.map((close, index) => skillProbeBound(close, `owner ${index}`)))
  for (const result of closures) {
    if (result.status === 'rejected')
      failures.push(result.reason)
  }
  try {
    await skillProbeBound(control.finished, 'final finite HTTP closure')
  }
  catch (cause) {
    failures.push(cause)
  }
  // Audits follow the drain AND producer closure, even when either is unknown.
  try {
    control.audit()
  }
  catch (cause) {
    failures.push(cause)
  }
  if (failures.length)
    throw new AggregateError(failures, 'Skill probe closure or audit failed')
}

for (const boundary of ['pending-poll', 'departing-timer'] as const) {
  test(`availability native reload probe (${boundary})`, async ({ page }, testInfo) => {
    const journal: Array<{ kind: string, hostAt: number, browserAt?: number, document?: string, poll?: number }> = []
    const errors: string[] = []
    const bodyFailures: Array<{ kind: string, document?: string, poll?: number }> = []
    const journalPath = testInfo.outputPath('availability-boundaries.json')
    const saveJournal = () => saveAvailabilityJournal(journalPath, JSON.stringify({ boundary, journal, errors, bodyFailures }))
    const record = (kind: string, browserAt?: number, document?: string, poll?: number) => {
      if (kind.endsWith('-body-rejected'))
        bodyFailures.push({ kind, document, poll })
      journal.push({ kind, hostAt: Date.now(), browserAt, document, poll })
      saveJournal()
    }
    const requests = new Map<import('@playwright/test').Request, 'pending' | 'finished' | 'cancelled' | 'failed'>()
    const statuses = new Map<import('@playwright/test').Request, number>()
    const finiteRequest = (request: import('@playwright/test').Request) => {
      const path = new URL(request.url()).pathname
      return ['fetch', 'xhr'].includes(request.resourceType()) && (path.startsWith('/api/') || path === '/web-build.json')
    }
    page.on('request', (request) => {
      if (finiteRequest(request))
        requests.set(request, 'pending')
    })
    page.on('response', (response) => {
      const request = response.request()
      if (finiteRequest(request)) {
        statuses.set(request, response.status())
        record(`network-poll-status-${response.status()}`)
      }
    })
    page.on('requestfinished', (request) => {
      if (requests.has(request)) {
        requests.set(request, 'finished')
        record('network-poll-finished')
      }
    })
    page.on('requestfailed', (request) => {
      if (requests.has(request)) {
        const cancelled = request.failure()?.errorText === 'Load request cancelled'
        requests.set(request, cancelled ? 'cancelled' : 'failed')
        record(cancelled ? 'network-poll-cancelled' : 'network-poll-failed')
      }
    })
    const releasePoll = Promise.withResolvers<void>()
    const releaseReload = Promise.withResolvers<void>()
    let pollIntercepted = false
    let targetPoll: import('@playwright/test').Request | undefined
    let deliveryFailure: unknown
    let pollHeld = false
    let reloadHeld = false
    let routeFailure: unknown
    let reloadFailure: unknown
    let reload: Promise<unknown> | undefined
    let departingDocument: string | undefined
    let earlierPolls = 0
    let pollReleased = false
    const primaryFailures: unknown[] = []
    const cleanupFailures: unknown[] = []
    const releaseHeldPoll = () => {
      pollReleased = true
      releasePoll.resolve()
    }
    const reached = async (kind: 'poll' | 'reload') => {
      await expect.poll(() => {
        if (routeFailure)
          throw routeFailure
        if (reloadFailure)
          throw reloadFailure
        return kind === 'poll' ? pollHeld : reloadHeld
      }, { message: `Reach the ${kind} response boundary` }).toBe(true)
    }
    const browserSettled = async () => {
      await expect.poll(() => page.evaluate(() => (window as any).shellPending.size), {
        message: 'Current document finite HTTP work and body continuations have settled',
      }).toBe(0)
      // This round trip follows the completed fetch microtasks and their native diagnostics.
      await page.evaluate(() => {})
      expect(bodyFailures).toEqual([])
    }
    record('setup-entered')
    page.on('pageerror', (error) => {
      errors.push(error.message)
      record('page-error')
    })
    try {
      await page.exposeFunction('availabilityProbe', record)
      await page.addInitScript(() => {
        const documentId = crypto.randomUUID()
        let pollId = 0
        const pending = new Set<number>()
        const consuming = new Set<number>()
        ;(window as any).shellPending = pending
        ;(window as any).shellObservation = { document: documentId, pending }
        const completions = new MessageChannel()
        completions.port1.onmessage = async (event) => {
          const { kind, poll } = event.data as { kind: string, poll: number }
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          await probe(kind, poll)
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          pending.delete(poll)
        }
        const probe = (kind: string, poll?: number) => {
          return (window as any).availabilityProbe(kind, performance.now(), documentId, poll) as Promise<void>
        }
        for (const kind of ['beforeunload', 'pagehide', 'pageshow'])
          window.addEventListener(kind, () => probe(kind))
        const fetch = window.fetch.bind(window)
        window.fetch = (...args) => {
          const input = args[0]
          const url = typeof input === 'string' || input instanceof URL ? String(input) : input.url
          const path = new URL(url, location.href).pathname
          if (!path.startsWith('/api/') && path !== '/web-build.json')
            return fetch(...args)
          const category = path === '/api/installations' ? 'poll' : 'http'
          const poll = ++pollId
          pending.add(poll)
          probe(`${category}-start`, poll)
          args[1]?.signal?.addEventListener('abort', () => probe(`${category}-abort`, poll), { once: true })
          const settled = (kind: string) => {
            // A posted task follows the application's await/catch/finally microtasks.
            // Body consumers keep this cycle pending through their continuation.
            completions.port2.postMessage({ kind: `${category}-${kind}`, poll })
          }
          try {
            return fetch(...args).then((response) => {
              for (const method of ['json', 'text', 'arrayBuffer', 'blob', 'formData'] as const) {
                const consume = response[method].bind(response)
                ;(response as any)[method] = (...options: unknown[]) => {
                  consuming.add(poll)
                  pending.add(poll)
                  return Reflect.apply(consume, response, options).then((body: unknown) => {
                    settled('body-resolved')
                    return body
                  }, (cause: unknown) => {
                    settled('body-rejected')
                    throw cause
                  })
                }
              }
              settled('headers')
              return response
            }, (cause) => {
              settled('rejected')
              throw cause
            })
          }
          catch (cause) {
            settled('threw')
            throw cause
          }
        }
      })
      await page.clock.install()
      await useRelayForHttpMocks(page)
      await page.goto(workspacePath('/skills'))
      const accountView = page.getByLabel('Email address').or(page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true }))
      await expect(accountView.first()).toBeAttached()
      await browserSettled()
      await authenticateWorkspace(page)
      await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
      await page.clock.pauseAt(new Date(Date.now() + 1000))
      await browserSettled()
      departingDocument = await page.evaluate(() => (window as any).shellObservation.document)
      earlierPolls = journal.filter(event => event.document === departingDocument && event.kind === 'poll-start').length
      await page.route('**/api/installations', async (route) => {
        // Hold exactly the old document's targeted request; the new document stays real.
        if (pollIntercepted)
          return route.continue()
        pollIntercepted = true
        targetPoll = route.request()
        try {
          const response = await route.fetch({ timeout: 7000 })
          record(`poll-response-status-${response.status()}`)
          expect(response.status()).toBe(200)
          pollHeld = true
          record('poll-response-held')
          await releasePoll.promise
          try {
            await route.fulfill({ response })
            record('poll-response-delivered')
          }
          catch (cause) {
            deliveryFailure = cause
            record('poll-response-not-delivered')
          }
        }
        catch (cause) {
          routeFailure = cause
          record('poll-route-failed')
          await route.abort().catch(() => {})
        }
      })
      await page.route('**/skills', async (route) => {
        if (!route.request().isNavigationRequest())
          return route.continue()
        try {
          const response = await route.fetch({ timeout: 7000 })
          record(`reload-response-status-${response.status()}`)
          expect(response.status()).toBe(200)
          reloadHeld = true
          record('reload-response-held')
          await releaseReload.promise
          await route.fulfill({ response })
          record('reload-response-delivered')
        }
        catch (cause) {
          routeFailure = cause
          record('reload-route-failed')
          await route.abort().catch(() => {})
        }
      })
      if (boundary === 'pending-poll') {
        await page.clock.runFor(3100)
        await reached('poll')
      }
      record('reload-entered')
      reload = page.reload()
      void reload.catch((cause) => {
        reloadFailure = cause
        record('reload-failed')
      })
      await reached('reload')
      if (boundary === 'departing-timer') {
        await page.clock.runFor(3100)
        await reached('poll')
      }
      // Keep both bodies held until the exact old read and old document settle.
      expect(pollHeld).toBe(true)
      await expect.poll(() => journal.filter(event => event.document === departingDocument && event.kind === 'poll-start').length).toBe(earlierPolls + 1)
      const target = journal.filter(event => event.document === departingDocument && event.kind === 'poll-start').at(-1)!.poll
      if (!departingDocument || typeof target !== 'number')
        throw new Error('Missing old skill document/read identity')
      const terminals = () => journal.filter(event => event.document === departingDocument && event.poll === target
        && ['poll-rejected', 'poll-threw', 'poll-body-rejected', 'poll-body-resolved'].includes(event.kind))
      await commitSkillReload({
        document: departingDocument,
        read: target,
        held: () => pollHeld && !pollReleased,
        snapshot: () => page.evaluate(() => ({ document: (window as any).shellObservation.document, pending: (window as any).shellPending.size })),
        terminals: async () => {
          await expect.poll(() => terminals().length).toBe(1)
          return terminals()
        },
        drain: browserSettled,
        advance: async () => {
          record('old-document-original-read-and-javascript-closed')
          releaseReload.resolve()
          await reload
          releaseHeldPoll()
        },
      })
      await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
    }
    catch (cause) {
      primaryFailures.push(cause)
      record('primary-failed')
    }
    finally {
      releaseHeldPoll()
      releaseReload.resolve()
      record('cleanup-entered')
      let cleanupCompleted = false
      try {
        await closeAndAuditSkillProbe({
          drain: browserSettled,
          close: [
            () => page.close(),
            () => page.unrouteAll({ behavior: 'wait' }),
            () => reload?.catch(() => {}) ?? Promise.resolve(),
          ],
          finished: async () => {
            await expect.poll(() => [...requests.values()].every(state => state !== 'pending'), {
              message: 'Every observed finite application HTTP request has terminated after producer closure',
            }).toBe(true)
          },
          audit: () => {
            const failures: unknown[] = []
            const audits = [
              () => {
                for (const [request, terminal] of requests) {
                  if (request === targetPoll)
                    record(`target-network-${terminal}`)
                  const status = statuses.get(request)
                  const successfulResponse = status !== undefined && status >= 200 && status < 300
                  if (terminal === 'pending')
                    throw new Error('Finite HTTP closure remains unknown')
                  if (terminal === 'finished' && !successfulResponse)
                    throw new Error(`An observed finite HTTP response has unexpected status ${status ?? 'unknown'}`)
                  if (terminal === 'failed')
                    throw new Error('An observed finite HTTP request failed unexpectedly')
                  if (terminal === 'cancelled' && request !== targetPoll)
                    throw new Error('A finite HTTP request outside the old-document target was cancelled')
                }
              },
              () => {
                if (deliveryFailure && (!targetPoll || requests.get(targetPoll) !== 'cancelled'))
                  throw deliveryFailure
              },
              () => {
                if (routeFailure)
                  throw routeFailure
              },
              () => {
                if (reloadFailure)
                  throw reloadFailure
              },
              () => expect(bodyFailures).toEqual([]),
              () => expect(errors).toEqual([]),
            ]
            for (const audit of audits) {
              try {
                audit()
              }
              catch (cause) {
                failures.push(cause)
              }
            }
            if (failures.length)
              throw new AggregateError(failures, 'Skill probe final audits failed')
          },
        })
        cleanupCompleted = true
      }
      catch (cause) {
        cleanupFailures.push(cause)
      }
      finally {
        record(cleanupCompleted ? 'cleanup-completed' : 'cleanup-failed')
        try {
          await testInfo.attach('availability-boundaries', { path: journalPath, contentType: 'application/json' })
        }
        catch (cause) {
          cleanupFailures.push(cause)
        }
      }
    }
    if (primaryFailures.length || cleanupFailures.length)
      throw new AggregateError([...primaryFailures, ...cleanupFailures], 'Skill probe failed')
  })
}
