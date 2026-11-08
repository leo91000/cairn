
for (const boundary of ['pending-poll', 'departing-timer'] as const) {
  test(`availability native reload probe (${boundary})`, async ({ page }, testInfo) => {
    const journal: Array<{ kind: string, at: number }> = []
    const errors: string[] = []
    await page.exposeFunction('availabilityProbe', (kind: string, at: number) => journal.push({ kind, at }))
    await page.addInitScript(() => {
      const probe = (kind: string) => {
        void (window as any).availabilityProbe(kind, performance.now())
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
    page.on('pageerror', error => errors.push(error.message))
    await page.clock.install()
    await useRelayForHttpMocks(page)
    await page.goto(workspacePath('/skills'))
    await authenticateWorkspace(page)
    await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
    await page.clock.pauseAt(new Date(Date.now() + 1000))
    const pollHeld = Promise.withResolvers<void>()
    const releasePoll = Promise.withResolvers<void>()
    const reloadHeld = Promise.withResolvers<void>()
    const releaseReload = Promise.withResolvers<void>()
    await page.route('**/api/installations', async (route) => {
      const response = await route.fetch()
      journal.push({ kind: 'poll-response-held', at: Date.now() })
      pollHeld.resolve()
      await releasePoll.promise
      await route.fulfill({ response }).catch(() => {})
    })
    await page.route('**/skills', async (route) => {
      if (!route.request().isNavigationRequest())
        return route.continue()
      const response = await route.fetch()
      journal.push({ kind: 'reload-response-held', at: Date.now() })
      reloadHeld.resolve()
      await releaseReload.promise
      await route.fulfill({ response }).catch(() => {})
    })
    let reload: Promise<unknown> | undefined
    try {
      if (boundary === 'pending-poll') {
        await page.clock.runFor(3100)
        await pollHeld.promise
      }
      reload = page.reload()
      await reloadHeld.promise
      if (boundary === 'departing-timer') {
        await page.clock.runFor(3100)
        await pollHeld.promise
      }
      releaseReload.resolve()
      await reload
      releasePoll.resolve()
      await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
      expect(errors).toEqual([])
    }
    finally {
      releasePoll.resolve()
      releaseReload.resolve()
      await reload?.catch(() => {})
      await page.unrouteAll({ behavior: 'wait' })
      await testInfo.attach('availability-boundaries', { body: JSON.stringify({ boundary, journal, errors }), contentType: 'application/json' })
    }
  })
}
