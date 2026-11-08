
import { writeFileSync as saveAvailabilityJournal } from 'node:fs'

for (const boundary of ['pending-poll', 'departing-timer'] as const) {
  test(`availability native reload probe (${boundary})`, async ({ page }, testInfo) => {
    const journal: Array<{ kind: string, hostAt: number, browserAt?: number, document?: string }> = []
    const errors: string[] = []
    const journalPath = testInfo.outputPath('availability-boundaries.json')
    const saveJournal = () => saveAvailabilityJournal(journalPath, JSON.stringify({ boundary, journal, errors }))
    const record = (kind: string, browserAt?: number, document?: string) => {
      journal.push({ kind, hostAt: Date.now(), browserAt, document })
      saveJournal()
    }
    const releasePoll = Promise.withResolvers<void>()
    const releaseReload = Promise.withResolvers<void>()
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
    record('setup-entered')
    page.on('pageerror', (error) => {
      errors.push(error.message)
      record('page-error')
    })
    try {
      await page.exposeFunction('availabilityProbe', record)
      await page.addInitScript(() => {
        const documentId = crypto.randomUUID()
        const probe = (kind: string) => {
          void (window as any).availabilityProbe(kind, performance.now(), documentId)
        }
        for (const kind of ['beforeunload', 'pagehide', 'pageshow'])
          window.addEventListener(kind, () => probe(kind))
        const fetch = window.fetch.bind(window)
        window.fetch = (...args) => {
          if (args[0] === '/api/installations') {
            probe('poll-start')
            args[1]?.signal?.addEventListener('abort', () => probe('poll-abort'), { once: true })
          }
          return fetch(...args)
        }
      })
      await page.clock.install()
      await useRelayForHttpMocks(page)
      await page.goto(workspacePath('/skills'))
      await authenticateWorkspace(page)
      await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
      await page.clock.pauseAt(new Date(Date.now() + 1000))
      await page.route('**/api/installations', async (route) => {
        try {
          const response = await route.fetch({ timeout: 7000 })
          pollHeld = true
          record('poll-response-held')
          await releasePoll.promise
          try {
            await route.fulfill({ response })
            record('poll-response-delivered')
          }
          catch {
            // The departing document can cancel its already observed request.
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
