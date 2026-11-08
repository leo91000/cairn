
import { writeFileSync as saveAvailabilityJournal } from 'node:fs'

for (const boundary of ['pending-poll', 'departing-timer'] as const) {
  test(`availability native reload probe (${boundary})`, async ({ page }, testInfo) => {
    const journal: Array<{ kind: string, hostAt: number, browserAt?: number, document?: string, poll?: number }> = []
    const errors: string[] = []
    const journalPath = testInfo.outputPath('availability-boundaries.json')
    const saveJournal = () => saveAvailabilityJournal(journalPath, JSON.stringify({ boundary, journal, errors }))
    const record = (kind: string, browserAt?: number, document?: string, poll?: number) => {
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
        const completions = new MessageChannel()
        completions.port1.onmessage = (event) => {
          const { kind, poll } = event.data as { kind: string, poll: number }
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          pending.delete(poll)
          probe(kind, poll)
        }
        const probe = (kind: string, poll?: number) => {
          void (window as any).availabilityProbe(kind, performance.now(), documentId, poll)
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
      // Both stimuli must reach the old document before the reload can commit.
      expect(pollHeld).toBe(true)
      releaseReload.resolve()
      await reload
      releasePoll.resolve()
      await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
    }
    finally {
      releasePoll.resolve()
      releaseReload.resolve()
      record('cleanup-entered')
      let cleanupCompleted = false
      try {
        await reload?.catch(() => {})
        await page.unrouteAll({ behavior: 'wait' })
        await expect.poll(() => [...requests.values()].every(state => state !== 'pending'), {
          message: 'Every observed finite application HTTP request has terminated',
        }).toBe(true)
        for (const [request, terminal] of requests) {
          if (request === targetPoll)
            record(`target-network-${terminal}`)
          const status = statuses.get(request)
          const successfulResponse = status !== undefined && status >= 200 && status < 300
          if (terminal === 'finished' && !successfulResponse)
            throw new Error(`An observed finite HTTP response has unexpected status ${status ?? 'unknown'}`)
          if (terminal === 'failed')
            throw new Error('An observed finite HTTP request failed unexpectedly')
          if (terminal === 'cancelled' && request !== targetPoll)
            throw new Error('A finite HTTP request outside the old-document target was cancelled')
        }
        if (deliveryFailure && (!targetPoll || requests.get(targetPoll) !== 'cancelled'))
          throw deliveryFailure
        await browserSettled()
        cleanupCompleted = true
      }
      finally {
        record(cleanupCompleted ? 'cleanup-completed' : 'cleanup-failed')
        await testInfo.attach('availability-boundaries', { path: journalPath, contentType: 'application/json' })
      }
    }
    if (routeFailure)
      throw routeFailure
    expect(errors).toEqual([])
  })
}
